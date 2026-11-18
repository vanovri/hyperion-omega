"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.OMEGA_GOVERNANCE = exports.SettlementL2Proto = exports.WasmQuantProto = exports.MarginRiskProto = exports.ZkSolvencyProto = exports.ConsensusProto = void 0;
exports.ConsensusProto = __importStar(require("./generated/omega/consensus"));
exports.ZkSolvencyProto = __importStar(require("./generated/omega/zk_solvency"));
exports.MarginRiskProto = __importStar(require("./generated/omega/margin_risk"));
exports.WasmQuantProto = __importStar(require("./generated/omega/wasm_quant"));
exports.SettlementL2Proto = __importStar(require("./generated/omega/settlement_l2"));
exports.OMEGA_GOVERNANCE = {
    WASM_MAX_LINEAR_MEMORY_BYTES: 16 * 1024 * 1024,
    WASM_MAX_STACK_BYTES: 512 * 1024,
    WASM_DEFAULT_FUEL_PER_TICK: 50000n,
    WASM_MAX_EPOCH_WALLTIME_NS: 100000n,
    L2_MIN_FINALITY_BLOCKS: 12,
    HYPERION_L2_CANONICAL_CHAIN_ID: 994201n,
    FP_SCALE: 100000000n,
    PPM_SCALE: 1000000n,
};
//# sourceMappingURL=index.js.map