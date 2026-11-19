#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::{broadcast, Notify};

use crate::error::ChronosError;
use crate::pb::{
    market_event_envelope::Payload as EnvelopePayload, ControlReplaySessionRequest,
    ControlReplaySessionResponse, ReplayControlAction, ReplayFrameStream,
    StreamReplayFramesRequest,
};
use crate::state::ExchangeState;
use crate::wal::IndexedWalEvent;
use crate::ChronosEngine;

pub const MIN_SPEED_MILLI: u32 = 100; // 0.1x slow-motion
pub const DEFAULT_SPEED_MILLI: u32 = 1_000; // 1.0x real-time
pub const MAX_SPEED_MILLI: u32 = 100_000; // 100.0x fast-forward

#[inline]
pub fn clamp_speed_milli(raw: u32) -> u32 {
    if raw == 0 {
        DEFAULT_SPEED_MILLI
    } else {
        raw.clamp(MIN_SPEED_MILLI, MAX_SPEED_MILLI)
    }
}

#[derive(Debug, Clone)]
struct SessionMutableState {
    cursor_idx: usize,
    current_engine_seq: u64,
    speed_milli: u32,
    is_paused: bool,
    frame_index: u64,
    terminated: bool,
}

pub struct PlaybackSession {
    pub session_id: String,
    pub timeline_id: String,
    pub symbol: String,
    pub from_seq: u64,
    pub to_seq: u64,
    pub l2_depth: usize,
    inner: Mutex<SessionMutableState>,
    wakeup: Notify,
    tx: broadcast::Sender<ReplayFrameStream>,
}

/// Manages concurrent interactive VCR playback sessions across gRPC and HTTP/SSE consumers.
pub struct PlaybackSessionManager {
    engine: Arc<ChronosEngine>,
    sessions: RwLock<HashMap<String, Arc<PlaybackSession>>>,
    active_count: std::sync::atomic::AtomicU64,
}

impl PlaybackSessionManager {
    pub fn new(engine: Arc<ChronosEngine>) -> Self {
        Self {
            engine,
            sessions: RwLock::new(HashMap::new()),
            active_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn active_sessions_count(&self) -> u64 {
        self.active_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Starts (or attaches to) an interactive VCR playback session and returns a broadcast receiver
    /// of `ReplayFrameStream` frames.
    pub fn start_session(
        self: &Arc<Self>,
        req: StreamReplayFramesRequest,
    ) -> Result<(String, broadcast::Receiver<ReplayFrameStream>), ChronosError> {
        let session_id = if req.session_id.is_empty() {
            let n = self
                .active_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            format!("vcr-{n:06}")
        } else {
            req.session_id.clone()
        };
        let timeline_id = if req.timeline_id.is_empty() {
            "baseline".to_string()
        } else {
            req.timeline_id.clone()
        };
        let symbol = if req.symbol.is_empty() {
            "BTC-USDT".to_string()
        } else {
            req.symbol.clone()
        };
        let speed_milli = clamp_speed_milli(req.initial_speed_milli);
        let l2_depth = if req.l2_depth == 0 {
            20
        } else {
            req.l2_depth as usize
        };

        let (base_state, events, from_seq, to_seq) =
            self.load_timeline_window(&timeline_id, req.from_engine_seq, req.to_engine_seq)?;

        let (tx, rx) = broadcast::channel(512);
        let session = Arc::new(PlaybackSession {
            session_id: session_id.clone(),
            timeline_id: timeline_id.clone(),
            symbol: symbol.clone(),
            from_seq,
            to_seq,
            l2_depth,
            inner: Mutex::new(SessionMutableState {
                cursor_idx: 0,
                current_engine_seq: from_seq,
                speed_milli,
                is_paused: false,
                frame_index: 0,
                terminated: false,
            }),
            wakeup: Notify::new(),
            tx,
        });

        if let Ok(mut map) = self.sessions.write() {
            if let Some(old) = map.insert(session_id.clone(), Arc::clone(&session)) {
                if let Ok(mut g) = old.inner.lock() {
                    g.terminated = true;
                }
                old.wakeup.notify_waiters();
            } else {
                self.active_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let mgr = Arc::clone(self);
        let sess_clone = Arc::clone(&session);
        tokio::spawn(async move {
            mgr.run_vcr_loop(sess_clone, base_state, events).await;
        });

        Ok((session_id, rx))
    }

    /// Applies an interactive VCR transport command (`Play`, `Pause`, `StepForward`, `StepBackward`,
    /// `Rewind`, `SetSpeed`) to a live session.
    pub fn control_session(
        &self,
        req: &ControlReplaySessionRequest,
    ) -> Result<ControlReplaySessionResponse, ChronosError> {
        let session = self
            .sessions
            .read()
            .ok()
            .and_then(|m| m.get(&req.session_id).cloned())
            .ok_or_else(|| ChronosError::NotFound(format!("VCR session '{}' not found", req.session_id)))?;

        let (timeline_base, events, _, _) =
            self.load_timeline_window(&session.timeline_id, session.from_seq, session.to_seq)?;

        let action =
            ReplayControlAction::try_from(req.action).unwrap_or(ReplayControlAction::Unspecified);
        let steps = req.step_count.max(1) as usize;

        let mut st = session
            .inner
            .lock()
            .map_err(|_| ChronosError::InvalidArgument("session mutex poisoned".to_string()))?;

        match action {
            ReplayControlAction::Play => {
                if req.speed_milli > 0 {
                    st.speed_milli = clamp_speed_milli(req.speed_milli);
                }
                st.is_paused = false;
            }
            ReplayControlAction::Pause => {
                st.is_paused = true;
            }
            ReplayControlAction::SetSpeed => {
                st.speed_milli = clamp_speed_milli(req.speed_milli);
            }
            ReplayControlAction::StepForward => {
                st.is_paused = true;
                let target_idx = (st.cursor_idx + steps).min(events.len());
                st.cursor_idx = target_idx;
                let frame = build_frame_at_cursor(
                    &session,
                    &timeline_base,
                    &events,
                    st.cursor_idx,
                    st.frame_index,
                    st.speed_milli,
                    true,
                )?;
                st.current_engine_seq = frame.engine_seq;
                st.frame_index += 1;
                let _ = session.tx.send(frame);
            }
            ReplayControlAction::StepBackward => {
                st.is_paused = true;
                st.cursor_idx = st.cursor_idx.saturating_sub(steps);
                let frame = build_frame_at_cursor(
                    &session,
                    &timeline_base,
                    &events,
                    st.cursor_idx,
                    st.frame_index,
                    st.speed_milli,
                    true,
                )?;
                st.current_engine_seq = frame.engine_seq;
                st.frame_index += 1;
                let _ = session.tx.send(frame);
            }
            ReplayControlAction::Rewind => {
                st.is_paused = true;
                st.cursor_idx = 0;
                let frame = build_frame_at_cursor(
                    &session,
                    &timeline_base,
                    &events,
                    0,
                    st.frame_index,
                    st.speed_milli,
                    true,
                )?;
                st.current_engine_seq = frame.engine_seq;
                st.frame_index += 1;
                let _ = session.tx.send(frame);
            }
            ReplayControlAction::Unspecified => {
                return Err(ChronosError::InvalidArgument(
                    "ReplayControlAction must be specified".to_string(),
                ));
            }
        }

        let resp = ControlReplaySessionResponse {
            session_id: session.session_id.clone(),
            current_engine_seq: st.current_engine_seq,
            effective_speed_milli: st.speed_milli,
            is_paused: st.is_paused,
        };
        drop(st);
        session.wakeup.notify_waiters();
        Ok(resp)
    }

    fn load_timeline_window(
        &self,
        timeline_id: &str,
        req_from_seq: u64,
        req_to_seq: u64,
    ) -> Result<(ExchangeState, Vec<IndexedWalEvent>, u64, u64), ChronosError> {
        if timeline_id == "baseline" || timeline_id.is_empty() {
            let max_seq = self.engine.store().max_engine_seq();
            let from_seq = req_from_seq.min(max_seq);
            let to_seq = if req_to_seq == 0 {
                max_seq
            } else {
                req_to_seq.min(max_seq).max(from_seq)
            };
            let base_seq = from_seq.saturating_sub(1);
            let (base_state, _, _) = self.engine.reconstruct_state_at_seq(base_seq)?;
            let slice = self
                .engine
                .store()
                .events_in_seq_range(base_seq, to_seq)
                .to_vec();
            Ok((base_state, slice, from_seq, to_seq))
        } else {
            let fork = self.engine.get_fork(timeline_id).ok_or_else(|| {
                ChronosError::NotFound(format!("counterfactual timeline '{}' not found", timeline_id))
            })?;
            Ok((
                fork.base_state_at_start.clone(),
                fork.events.clone(),
                fork.start_engine_seq,
                fork.end_engine_seq,
            ))
        }
    }

    async fn run_vcr_loop(
        &self,
        session: Arc<PlaybackSession>,
        timeline_base: ExchangeState,
        events: Vec<IndexedWalEvent>,
    ) {
        let mut working_state = timeline_base.clone();
        let mut applied_cursor = 0usize;

        // Emit initial anchor frame if timeline is empty
        if events.is_empty() {
            if let Ok(frame) = build_frame_at_cursor(
                &session,
                &timeline_base,
                &events,
                0,
                0,
                DEFAULT_SPEED_MILLI,
                false,
            ) {
                let _ = session.tx.send(frame);
            }
            return;
        }

        loop {
            if session.tx.receiver_count() == 0 {
                break;
            }

            let (is_paused, cursor, speed_milli, frame_idx, terminated) = {
                let Ok(g) = session.inner.lock() else { break };
                (
                    g.is_paused,
                    g.cursor_idx,
                    g.speed_milli,
                    g.frame_index,
                    g.terminated,
                )
            };

            if terminated {
                break;
            }

            if is_paused || cursor >= events.len() {
                session.wakeup.notified().await;
                continue;
            }

            // Resync incremental `working_state` if operator stepped backward or rewound
            if cursor < applied_cursor {
                working_state = timeline_base.clone();
                applied_cursor = 0;
            }
            while applied_cursor < cursor {
                let _ = working_state.apply_wal_envelope(&events[applied_cursor].envelope);
                applied_cursor += 1;
            }

            // Apply next single event (`events[cursor]`)
            let ev = &events[cursor];
            let prev_ts_ns = working_state.ts_ns;
            let _ = working_state.apply_wal_envelope(&ev.envelope);
            applied_cursor = cursor + 1;

            let frame = build_frame_from_live_state(
                &session,
                &mut working_state,
                ev,
                frame_idx,
                speed_milli,
                false,
            );

            {
                let Ok(mut g) = session.inner.lock() else { break };
                // Only advance if an asynchronous Rewind/Pause didn't override cursor mid-flight
                if !g.is_paused && g.cursor_idx == cursor {
                    g.cursor_idx = cursor + 1;
                    g.current_engine_seq = ev.engine_seq;
                    g.frame_index = frame_idx + 1;
                } else {
                    continue;
                }
            }

            if session.tx.send(frame).is_err() {
                break;
            }

            // Variable-speed inter-event pacing (`speed_milli`: 100 = 0.1x, 1000 = 1.0x, 100000 = 100x)
            let raw_delta_ns = if prev_ts_ns > 0 && ev.ts_ns > prev_ts_ns {
                (ev.ts_ns - prev_ts_ns).min(50_000_000) // Cap idle gaps to 50ms raw so VCR never stalls on overnight gaps
            } else {
                1_000_000 // Default 1ms nominal tick spacing
            };
            let scaled_ns = ((raw_delta_ns as u128) * 1_000) / (speed_milli.max(1) as u128);
            let clamped_sleep_ns = scaled_ns.clamp(50_000, 250_000_000) as u64;

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_nanos(clamped_sleep_ns)) => {}
                _ = session.wakeup.notified() => {}
            }
        }

        if let Ok(mut map) = self.sessions.write() {
            map.remove(&session.session_id);
        }
    }
}

fn build_frame_at_cursor(
    session: &PlaybackSession,
    timeline_base: &ExchangeState,
    events: &[IndexedWalEvent],
    cursor_count: usize,
    frame_index: u64,
    speed_milli: u32,
    is_paused: bool,
) -> Result<ReplayFrameStream, ChronosError> {
    let mut st = timeline_base.clone();
    let limit = cursor_count.min(events.len());
    for ev in &events[..limit] {
        st.apply_wal_envelope(&ev.envelope)?;
    }

    if limit == 0 {
        let root = st.compute_state_root();
        let l2 = st.build_l2_snapshot(&session.symbol, session.l2_depth);
        return Ok(ReplayFrameStream {
            session_id: session.session_id.clone(),
            timeline_id: session.timeline_id.clone(),
            symbol: session.symbol.clone(),
            frame_index,
            engine_seq: st.engine_seq,
            event_ts_ns: st.ts_ns,
            speed_milli,
            is_paused,
            state_root_blake3: root.to_vec(),
            l2_snapshot: Some(l2),
            frame_deltas: Vec::new(),
            frame_fills: Vec::new(),
        });
    }

    let last_ev = &events[limit - 1];
    Ok(build_frame_from_live_state(
        session,
        &mut st,
        last_ev,
        frame_index,
        speed_milli,
        is_paused,
    ))
}

fn build_frame_from_live_state(
    session: &PlaybackSession,
    state: &mut ExchangeState,
    ev: &IndexedWalEvent,
    frame_index: u64,
    speed_milli: u32,
    is_paused: bool,
) -> ReplayFrameStream {
    let root = state.compute_state_root();
    let l2 = state.build_l2_snapshot(&session.symbol, session.l2_depth);
    let mut frame_deltas = Vec::new();
    let mut frame_fills = Vec::new();

    match &ev.envelope.payload {
        Some(EnvelopePayload::L3Delta(d)) => frame_deltas.push(d.clone()),
        Some(EnvelopePayload::Fill(f)) => frame_fills.push(f.clone()),
        Some(EnvelopePayload::OrderAck(ack)) => frame_fills.extend(ack.fills.iter().cloned()),
        _ => {}
    }

    ReplayFrameStream {
        session_id: session.session_id.clone(),
        timeline_id: session.timeline_id.clone(),
        symbol: session.symbol.clone(),
        frame_index,
        engine_seq: ev.engine_seq,
        event_ts_ns: ev.ts_ns,
        speed_milli,
        is_paused,
        state_root_blake3: root.to_vec(),
        l2_snapshot: Some(l2),
        frame_deltas,
        frame_fills,
    }
}
