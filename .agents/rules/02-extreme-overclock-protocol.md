# HYPERION — PROTOKOL OVERCLOCK EKSTREM (`MANDATORY QUALITY & PERFORMANCE GATE`)

- **Version**: `1.0.0` (Global Overclock Governor Mandate — Active Across `CH01`–`CH20`)
- **Applies To**: Seluruh modul kalkulasi inti, engine konkurensi/memori, dan dokumen handoff `specs/handoffs/chXX-*.md` pada fase **HYPERION** (`CH01`–`CH10`) dan **HYPERION OMEGA** (`CH11`–`CH20`).

Setiap channel **WAJIB** mengeksekusi dan mendokumentasikan ketiga gerbang verifikasi ekstrem di bawah ini sebelum menandai pekerjaannya selesai:

---

## 1. MUTATION SURVIVAL ZERO (`Mutation Score >= 95%`)
- Gunakan **`cargo-mutants`** (untuk Rust), **`mutmut`** (untuk Python), atau **injeksi mutasi terprogram / AST-operator mutator** pada modul kalkulasi finansial, kriptografi, konsensus, dan analitik inti (`>` $\leftrightarrow$ `>=`, `<` $\leftrightarrow$ `<=`, `+` $\leftrightarrow$ `-`, `*` $\leftrightarrow$ `/`, `==` $\leftrightarrow$ `!=`, batas skala fixed-point `100_000_000` / `1_000_000`, tanda fee/rebate, batas kuorum/margin).
- Jika ada mutan logika yang lolos (*survived*) tanpa menggagalkan unit test, **WAJIB** menambahkan test case deterministik baru sampai **Mutation Kill Score pada modul inti mencapai `>= 95.0%`** (target ideal `100%`, `0` mutan kritis bertahan).
- Cantumkan ringkasan jumlah mutan (`Killed / Total = XX.XX%`) di dokumen `specs/handoffs/chXX-*.md`.

---

## 2. UNDEFINED BEHAVIOR, RACE DETECTOR & `500,000`-CYCLE ZERO-LEAK SOAK TEST
- **Rust (`unsafe` / Arena / Slab / Lock-Free Ring Buffer / SIMD)**:
  - Jalankan verifikasi **Miri** (`cargo +nightly miri test`) atau **AddressSanitizer** bila memungkinkan pada modul murni non-FFI, dan audit ketat setiap invarian pointer/aliasing.
  - Jalankan **Soak Test `500.000` siklus berturut-turut** (alokasi + pencocokan/eksekusi + pembebasan slot arena/ring-buffer) dan buktikan **`0 byte` kebocoran memori steady-state** (kapasitas slab/arena/heap konstan setelah warmup).
- **Go (`gateway-go`, `chaos-sim`, dll.)**:
  - Jalankan **`go test -race ./...`** (`0` data race) dan **`runtime/pprof` / `runtime.ReadMemStats` memory profile**.
  - Jalankan **Soak Test `500.000` siklus berturut-turut** dan buktikan **`0 byte` kebocoran heap steady-state** (objek `sync.Pool` / koneksi / goroutine kembali ke baseline tanpa pertumbuhan tak terbatas).
- **Python / TypeScript / Wasm**:
  - Jalankan **`tracemalloc` / heap snapshot soak test `500.000` siklus** pada loop kalkulasi inti dengan pertumbuhan memori bocor steady-state `0 byte` setelah warmup.

---

## 3. MICRO-OPTIMIZATION FLAMEGRAPH LOOP (`>= 3 MEASURED PASSES`)
- Lakukan **minimal 3 putaran optimasi performa terukur** pada jalur panas (*hot path*):
  1. **Putaran 1 — `Baseline`**: Implementasi referensi fungsional awal yang benar secara matematis.
  2. **Putaran 2 — `Vectorized / Zero-Copy Refactor`**: Eliminasi alokasi heap/kloning string/buffer pada *hot path*, parsing zero-copy (`bytes`/`&[u8]`/flat slices), pre-allocated arena/pool, atau vektorisasi SIMD/SoA.
  3. **Putaran 3 — `Cache-Line Aligned Final`**: Penyelarasan baris cache CPU (`#[repr(C, align(64))]` / `CachePadded` untuk mencegah *false sharing* lintas core), branchless arithmetic, inline hint terarah, atau compact flat layout.
- **Tabel Wajib di Dokumen Handoff (`specs/handoffs/chXX-*.md`)**:
  Cantumkan tabel perbandingan jujur hasil pengukuran aktual (bukan angka karangan) berisi **Latensi (`ns/op` — p50 / p99)** dan **Alokasi Memori (`B/op` & `allocs/op`)** untuk ketiga putaran (**Baseline vs Vectorized/Zero-Copy vs Cache-Line Aligned Final**) beserta persentase peningkatannya!
