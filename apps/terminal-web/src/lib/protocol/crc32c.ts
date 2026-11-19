/**
 * Castagnoli CRC-32C (polynomial 0x1EDC6F41, reflected 0x82F63B78)
 * zero-allocation lookup table implementation for ws-binary-protocol.md frame verification.
 */

const CRC32C_POLY_REFLECTED = 0x82f63b78;

const CRC32C_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let crc = i;
    for (let bit = 0; bit < 8; bit++) {
      crc = (crc & 1) !== 0 ? (crc >>> 1) ^ CRC32C_POLY_REFLECTED : crc >>> 1;
    }
    table[i] = crc >>> 0;
  }
  return table;
})();

export function crc32c(bytes: Uint8Array, offset = 0, length = bytes.byteLength - offset): number {
  let crc = 0xffffffff;
  const end = offset + length;
  for (let i = offset; i < end; i++) {
    const byte = bytes[i] ?? 0;
    const tableIdx = (crc ^ byte) & 0xff;
    crc = (crc >>> 8) ^ (CRC32C_TABLE[tableIdx] ?? 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}
