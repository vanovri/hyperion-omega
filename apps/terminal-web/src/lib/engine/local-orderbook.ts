/**
 * HYPERION Zero-Gap Local L2/L3 Order Book & Sequence Continuity Engine
 * Runs strictly inside the Dedicated WebWorker.
 *
 * Implements ws-binary-protocol.md §3.3 Sequence Continuity Rules:
 * 1. prevEngineSeq == localSeq -> apply deltas, advance localSeq.
 * 2. prevEngineSeq < localSeq  -> skip already-applied itemEngineSeq <= localSeq (idempotent).
 * 3. prevEngineSeq > localSeq OR SLOW_CONSUMER_DROPPED_PRIOR -> enter awaitingResync mode,
 *    buffer incoming batches, and return needsResync = true so Worker emits 0x83 CMD_RESYNC_REQUEST.
 */

import { checkedNotionalFp, computeImbalancePpm, computeSpreadBps } from "../fixed-point";
import { L3_OP, WS_FLAGS, bytesToHex } from "../protocol/ws-binary-codec";
import type { L2SnapshotPayload, L3DeltaBatchPayload, L3DeltaItem } from "../protocol/ws-binary-codec";
import type { L2Level } from "../types";

interface RestingOrder {
  side: "BUY" | "SELL";
  priceFp: bigint;
  qtyFp: bigint;
}

interface AggregatedTick {
  totalQtyFp: bigint;
  orderCount: number;
}

export interface BookTopMetrics {
  localSeq: bigint;
  bestBidFp: bigint;
  bestAskFp: bigint;
  midPriceFp: bigint;
  spreadFp: bigint;
  spreadBps: number;
  imbalanceTop5Ppm: number;
  imbalanceTop10Ppm: number;
  imbalanceTop20Ppm: number;
  uncrossedHolds: boolean;
  stateRootBlake3Hex: string;
  bids: L2Level[];
  asks: L2Level[];
}

export class LocalOrderBook {
  public readonly instrumentId: number;
  private localSeq = 0n;
  private awaitingResync = true;
  private gapResyncCount = 0;
  private stateRootBlake3Hex = "0".repeat(64);

  private readonly orders = new Map<bigint, RestingOrder>();
  private readonly bidTicks = new Map<bigint, AggregatedTick>();
  private readonly askTicks = new Map<bigint, AggregatedTick>();
  private readonly bufferedBatches: L3DeltaBatchPayload[] = [];

  constructor(instrumentId: number) {
    this.instrumentId = instrumentId;
  }

  public get sequence(): bigint {
    return this.localSeq;
  }

  public get isAwaitingResync(): boolean {
    return this.awaitingResync;
  }

  public get resyncCount(): number {
    return this.gapResyncCount;
  }

  /** Reset and load full L2 snapshot, then replay any buffered post-snapshot deltas. */
  public applySnapshot(engineSeq: bigint, snapshot: L2SnapshotPayload): void {
    this.orders.clear();
    this.bidTicks.clear();
    this.askTicks.clear();

    for (const lvl of snapshot.bids) {
      if (lvl.totalQtyFp > 0n) {
        this.bidTicks.set(lvl.priceFp, {
          totalQtyFp: lvl.totalQtyFp,
          orderCount: Math.max(1, lvl.orderCount),
        });
      }
    }
    for (const lvl of snapshot.asks) {
      if (lvl.totalQtyFp > 0n) {
        this.askTicks.set(lvl.priceFp, {
          totalQtyFp: lvl.totalQtyFp,
          orderCount: Math.max(1, lvl.orderCount),
        });
      }
    }

    this.localSeq = engineSeq;
    this.stateRootBlake3Hex = bytesToHex(snapshot.stateRootBlake3, 32);
    this.awaitingResync = false;

    if (this.bufferedBatches.length > 0) {
      const pending = this.bufferedBatches.splice(0, this.bufferedBatches.length);
      pending.sort((a, b) => (a.prevEngineSeq < b.prevEngineSeq ? -1 : a.prevEngineSeq > b.prevEngineSeq ? 1 : 0));
      for (const batch of pending) {
        const lastDelta = batch.deltas[batch.deltas.length - 1];
        if (!lastDelta || lastDelta.itemEngineSeq <= this.localSeq) {
          continue;
        }
        if (batch.prevEngineSeq <= this.localSeq) {
          this.applyBatchInternal(batch);
        } else {
          // Still a gap after snapshot; request another resync
          this.awaitingResync = true;
          this.bufferedBatches.push(batch);
          break;
        }
      }
    }
  }

  /**
   * Apply an L3 delta batch. Returns `{ needsResync: true }` if a sequence gap or
   * slow-consumer drop flag was detected so the caller sends 0x83 CMD_RESYNC_REQUEST.
   */
  public applyDeltaBatch(batch: L3DeltaBatchPayload, headerFlags = 0): { needsResync: boolean } {
    const droppedFlag = (headerFlags & WS_FLAGS.SLOW_CONSUMER_DROPPED_PRIOR) !== 0;

    if (this.awaitingResync || droppedFlag || batch.prevEngineSeq > this.localSeq) {
      if (!this.awaitingResync) {
        this.awaitingResync = true;
        this.gapResyncCount++;
      }
      if (this.bufferedBatches.length < 256) {
        this.bufferedBatches.push(batch);
      }
      return { needsResync: true };
    }

    this.applyBatchInternal(batch);
    return { needsResync: false };
  }

  private applyBatchInternal(batch: L3DeltaBatchPayload): void {
    for (const delta of batch.deltas) {
      if (delta.itemEngineSeq <= this.localSeq) {
        continue; // Idempotent overlap skip
      }
      this.applySingleDelta(delta);
      this.localSeq = delta.itemEngineSeq;
    }
  }

  private addTickQty(side: "BUY" | "SELL", priceFp: bigint, qtyDeltaFp: bigint, orderDelta: number): void {
    const map = side === "BUY" ? this.bidTicks : this.askTicks;
    const existing = map.get(priceFp);
    if (!existing) {
      if (qtyDeltaFp > 0n) {
        map.set(priceFp, {
          totalQtyFp: qtyDeltaFp,
          orderCount: Math.max(1, orderDelta),
        });
      }
      return;
    }
    const nextQty = existing.totalQtyFp + qtyDeltaFp;
    if (nextQty <= 0n) {
      map.delete(priceFp);
    } else {
      existing.totalQtyFp = nextQty;
      existing.orderCount = Math.max(1, existing.orderCount + orderDelta);
    }
  }

  private applySingleDelta(delta: L3DeltaItem): void {
    if (delta.op === L3_OP.CLEAR) {
      if (delta.priceFp === 0n) {
        this.orders.clear();
        this.bidTicks.clear();
        this.askTicks.clear();
      } else {
        const map = delta.side === "BUY" ? this.bidTicks : this.askTicks;
        map.delete(delta.priceFp);
      }
      return;
    }

    const prevOrder = this.orders.get(delta.orderId);

    if (delta.op === L3_OP.ADD) {
      if (prevOrder) {
        this.addTickQty(prevOrder.side, prevOrder.priceFp, -prevOrder.qtyFp, -1);
      }
      if (delta.remainingQtyFp > 0n) {
        this.orders.set(delta.orderId, {
          side: delta.side,
          priceFp: delta.priceFp,
          qtyFp: delta.remainingQtyFp,
        });
        this.addTickQty(delta.side, delta.priceFp, delta.remainingQtyFp, 1);
      }
      return;
    }

    if (delta.op === L3_OP.MODIFY) {
      if (prevOrder) {
        this.addTickQty(prevOrder.side, prevOrder.priceFp, -prevOrder.qtyFp, -1);
        this.orders.delete(delta.orderId);
      }
      if (delta.remainingQtyFp > 0n) {
        this.orders.set(delta.orderId, {
          side: delta.side,
          priceFp: delta.priceFp,
          qtyFp: delta.remainingQtyFp,
        });
        this.addTickQty(delta.side, delta.priceFp, delta.remainingQtyFp, 1);
      }
      return;
    }

    if (delta.op === L3_OP.CANCEL || delta.op === L3_OP.FILL) {
      if (prevOrder) {
        const removedQty = prevOrder.qtyFp - delta.remainingQtyFp;
        if (delta.remainingQtyFp <= 0n) {
          this.addTickQty(prevOrder.side, prevOrder.priceFp, -prevOrder.qtyFp, -1);
          this.orders.delete(delta.orderId);
        } else {
          this.addTickQty(prevOrder.side, prevOrder.priceFp, -removedQty, 0);
          prevOrder.qtyFp = delta.remainingQtyFp;
        }
      } else {
        // Order rested before snapshot; apply synthetic level mutation from deltaQtySignFlags or remainingQtyFp
        const map = delta.side === "BUY" ? this.bidTicks : this.askTicks;
        const tick = map.get(delta.priceFp);
        if (tick) {
          if (delta.remainingQtyFp === 0n && tick.orderCount <= 1) {
            map.delete(delta.priceFp);
          } else if (delta.remainingQtyFp < tick.totalQtyFp) {
            tick.totalQtyFp = delta.remainingQtyFp > 0n ? delta.remainingQtyFp : 0n;
            if (tick.totalQtyFp <= 0n) map.delete(delta.priceFp);
          }
        }
      }
    }
  }

  /** Materialize sorted top-N L2 ladder and quantitative imbalance metrics for 60Hz frame emission. */
  public snapshotTopLevels(depth = 24): BookTopMetrics {
    const bidPrices = Array.from(this.bidTicks.keys()).sort((a, b) => (a > b ? -1 : a < b ? 1 : 0));
    const askPrices = Array.from(this.askTicks.keys()).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));

    // Enforce uncrossed display invariant if transient non-atomic delta arrived
    while (bidPrices.length > 0 && askPrices.length > 0 && (bidPrices[0] ?? 0n) >= (askPrices[0] ?? 0n)) {
      const crossedBid = bidPrices[0] ?? 0n;
      const crossedAsk = askPrices[0] ?? 0n;
      const bTick = this.bidTicks.get(crossedBid);
      const aTick = this.askTicks.get(crossedAsk);
      if ((bTick?.totalQtyFp ?? 0n) <= (aTick?.totalQtyFp ?? 0n)) {
        this.bidTicks.delete(crossedBid);
        bidPrices.shift();
      } else {
        this.askTicks.delete(crossedAsk);
        askPrices.shift();
      }
    }

    const bids: L2Level[] = [];
    let cumBidQty = 0n;
    let cumBidNotional = 0n;
    const bidLimit = Math.min(depth, bidPrices.length);
    for (let i = 0; i < bidLimit; i++) {
      const p = bidPrices[i]!;
      const t = this.bidTicks.get(p)!;
      cumBidQty += t.totalQtyFp;
      cumBidNotional += checkedNotionalFp(p, t.totalQtyFp);
      bids.push({
        priceFp: p,
        totalQtyFp: t.totalQtyFp,
        cumulativeQtyFp: cumBidQty,
        cumulativeNotionalFp: cumBidNotional,
        orderCount: t.orderCount,
      });
    }

    const asks: L2Level[] = [];
    let cumAskQty = 0n;
    let cumAskNotional = 0n;
    const askLimit = Math.min(depth, askPrices.length);
    for (let i = 0; i < askLimit; i++) {
      const p = askPrices[i]!;
      const t = this.askTicks.get(p)!;
      cumAskQty += t.totalQtyFp;
      cumAskNotional += checkedNotionalFp(p, t.totalQtyFp);
      asks.push({
        priceFp: p,
        totalQtyFp: t.totalQtyFp,
        cumulativeQtyFp: cumAskQty,
        cumulativeNotionalFp: cumAskNotional,
        orderCount: t.orderCount,
      });
    }

    const bestBidFp = bids[0]?.priceFp ?? 0n;
    const bestAskFp = asks[0]?.priceFp ?? 0n;
    const midPriceFp =
      bestBidFp > 0n && bestAskFp > 0n ? (bestBidFp + bestAskFp) / 2n : bestBidFp > 0n ? bestBidFp : bestAskFp;
    const spreadFp = bestBidFp > 0n && bestAskFp > bestBidFp ? bestAskFp - bestBidFp : 0n;
    const spreadBps = computeSpreadBps(bestBidFp, bestAskFp);

    const sumLevels = (arr: L2Level[], k: number): bigint => {
      let total = 0n;
      const n = Math.min(k, arr.length);
      for (let i = 0; i < n; i++) total += arr[i]!.totalQtyFp;
      return total;
    };

    const uncrossedHolds = bestBidFp === 0n || bestAskFp === 0n || bestBidFp < bestAskFp;

    return {
      localSeq: this.localSeq,
      bestBidFp,
      bestAskFp,
      midPriceFp,
      spreadFp,
      spreadBps,
      imbalanceTop5Ppm: Number(computeImbalancePpm(sumLevels(bids, 5), sumLevels(asks, 5))),
      imbalanceTop10Ppm: Number(computeImbalancePpm(sumLevels(bids, 10), sumLevels(asks, 10))),
      imbalanceTop20Ppm: Number(computeImbalancePpm(sumLevels(bids, 20), sumLevels(asks, 20))),
      uncrossedHolds,
      stateRootBlake3Hex: this.stateRootBlake3Hex,
      bids,
      asks,
    };
  }
}
