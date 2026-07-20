# CR Rust Project — Handoff Document

## Project Location
`B:\Code\cr_rust\`

## What This Project Does
A GUI tool with two buttons that replaces 3 VB.NET console applications:
1. **Extract XML** (port of `B:\Code\CR\Module1.vb`) — reads a CIM RDF/XML file, extracts all element types to per-type CSV files. **WORKING** — completes in ~13 seconds.
2. **Build Model** (port of `B:\Code\CR2\Module1.vb`) — reads ~35 CSV files, joins them, builds a PSSE power system model, writes .raw files + ~25 output CSVs. **WORKING** — completes in **6.7 seconds** with May 2026 ML1 data.
3. **Contingency processing** (port of `B:\Code\Contingency Test\Module1.vb`) — post-processing within Build Model. **WORKING** — verified against VB source.

## Build Status
```
cargo build --release   →  0 errors, 0 warnings
```
Binary: `target/release/cr.exe` (4.8 MB)

## Performance
| Version | Time | Speedup |
|---------|------|---------|
| Original VB.NET | ~4 hours | 1x |
| Rust (before optimization) | 320 seconds | 45x |
| Rust (after optimization) | **6.7 seconds** | **2100x** |

### Timing breakdown (May 2026 ML1 data, 200K terminals, 54K disconnectors):
| Step | Time |
|------|------|
| CSV loading | 2.6s |
| Joins (6 selective + 5 full) | 1.4s |
| Output table building | 1.6s |
| Disconnector elimination | 0.2s |
| Hub data + .raw writing + contingency | 0.9s |

## All Fixes Applied

### Phase 1: Compilation Fixes (from incomplete Rc<str> migration)
1. `s()` → `rc()` rename — 15 call sites
2. `rd()` → `rc()` — 3 calls to non-existent function
3. `rc(m)()` → `rc(m)` — 2 calls tried to invoke Rc<str> as function
4. `.into()` ambiguity — changed `s()` to `impl AsRef<str>`, replaced 128 `.into()` with `.to_string()`
5. Removed unnecessary `mut` from 14 variables
6. Suppressed dead-code warnings

### Phase 2: OOM Crash Fixes
7. **`s()` guard for `usize::MAX`** — prevented instant OOM (18 exabytes) on failed column lookup
8. **`s_rc()` method** — O(1) Rc clone writes
9. **`index()` uses `Rc::clone()`** — eliminates millions of String allocations
10. **Join functions use `gr()` + `s_rc()`** — 0 allocations per cell copy
11. **`write_csv()` optimized** — direct byte writes
12. **~100+ `g()` → `gr()` conversions** in all hot loops

### Phase 3: Logic Bug Fixes (found during runtime testing)
13. **`cim:ACLinesegment` → `cim:ACLineSegment`** — case mismatch caused 3.9 billion wasted iterations
14. **Moved `al1_pt_idx` outside loop** — was building 488K-entry index 2.8K times

### Phase 4: Disconnector Elimination Optimization
15. **Replaced O(N×M) scan with HashMap-indexed lookup** — built bus→rows indexes for linedata, xmrdata, loaddata, gendata. Each disconnector now only touches the ~5 rows that actually reference the discarded bus, instead of scanning all 84K rows. Reduced from 4.5 billion iterations to ~270K.
16. **Replaced O(D²) discard list with HashMap + reverse map** — O(1) chaining updates instead of scanning all previous discards.

## Key Design Decisions

### Table struct (Rc<str> for O(1) cloning):
```rust
type S = Rc<str>;
struct Table {
    headers: Vec<S>,
    hmap: HashMap<S, usize>,
    rows: Vec<Vec<S>>,
}
```

### Disconnector elimination (HashMap-indexed):
```
Build indexes: bus_number → [row indices that reference it]
For each disconnector (r2 → r3):
    Look up r2 in index → get only matching rows (O(K), typically ~5)
    Update those rows, move index entries from r2 to r3
Discard mapping with reverse map for O(1) chaining
```

## File Sizes
- processor.rs: ~985 lines (working)
- app.rs: ~186 lines (working)
- main.rs: ~22 lines (working)
- model_builder.rs: ~1230 lines (working)

## Build
```
cargo build --release
```
Binary: `target/release/cr.exe` (4.8 MB)

## Input/Output Paths
- Extract: XML file → output folder (CSVs)
- Build Model: input folder (CSVs) → `input/Output/` (model files)
- Both configurable in GUI, pre-filled with `B:\ERCOT\CIM Data\...` defaults