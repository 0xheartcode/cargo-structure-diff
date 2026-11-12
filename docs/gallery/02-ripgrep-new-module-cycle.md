<!-- csd-structure-gate -->
## Structure gate: passed

### Findings

- [warn] cycles: module cycle: ignore -> ignore::dir -> ignore::gitignore -> ignore::incremental -> ignore::overrides -> ignore::pathutil -> ignore::types -> ignore::walk

<details><summary>Changelog</summary>

# Structure changelog vs c55818fd32d6

## ignore::dir::IgnoreBuilder

### Added
- fn `is_hidden`

## ignore::incremental (added)

### Added
- enum `CachedDir`
- struct `IncrementalIgnore`
- struct `IncrementalIgnoreOptions`
- struct `IncrementalMatch`
- enum `RootIgnore`
- fn `normalize_absolute`

## ignore::incremental::CachedDir

### Added
- variant `Allowed`
- variant `Ignored`

## ignore::incremental::IncrementalIgnore

### Added
- fn `match_path`
- fn `matched`
- fn `matched_with_errors`
- fn `matched_with_errors_ignore`
- fn `matched_with_errors_impl`
- fn `new`
- fn `normalize`
- fn `root`
- fn `root_ignore`

## ignore::incremental::IncrementalMatch

### Added
- fn `from_match`
- fn `ignore`
- fn `invert`
- fn `is_ignore`
- fn `is_none`
- fn `is_whitelist`
- fn `is_within_depth`
- fn `no_descent`
- fn `none`
- fn `not_within_depth`
- fn `should_descend`

## ignore::incremental::RootIgnore

### Added
- variant `Loaded`
- variant `NotDirectory`
- variant `Stdin`
- variant `Unloaded`

## ignore::walk::WalkBuilder

### Added
- fn `build_ignore`
- fn `build_matchers`

### Modified
- fn `build`
- fn `build_parallel`

## Dependencies

- + `ignore` -> `ignore::incremental` (uses)
- + `ignore::incremental` -> `ignore` (uses)
- + `ignore::incremental` -> `ignore::dir` (uses)
- + `ignore::incremental` -> `ignore::pathutil` (uses)
- + `ignore::incremental::IncrementalIgnore` -> `ignore::incremental::IncrementalIgnoreOptions` (associates)
- + `ignore::incremental::IncrementalIgnore` -> `ignore::incremental::RootIgnore` (associates)
- + `ignore::incremental::IncrementalIgnore::matched` -> `ignore::incremental::IncrementalIgnore::matched_with_errors` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors` -> `ignore::incremental::IncrementalIgnore::matched_with_errors_impl` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors_ignore` -> `ignore::incremental::IncrementalIgnore::match_path` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors_ignore` -> `ignore::incremental::IncrementalIgnore::match_path` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors_ignore` -> `ignore::incremental::IncrementalIgnore::root_ignore` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors_impl` -> `ignore::incremental::IncrementalIgnore::matched_with_errors_ignore` (calls)
- + `ignore::incremental::IncrementalIgnore::matched_with_errors_impl` -> `ignore::incremental::IncrementalIgnore::root` (calls)
- + `ignore::incremental::IncrementalIgnore::normalize` -> `ignore::incremental::normalize_absolute` (calls)
- + `ignore::incremental::IncrementalIgnore::normalize` -> `ignore::incremental::normalize_absolute` (calls)
- + `ignore::walk` -> `ignore::incremental` (uses)
- + `ignore::walk::WalkBuilder::build` -> `ignore::walk::WalkBuilder::build_ignore` (calls)
- + `ignore::walk::WalkBuilder::build_ignore` -> `ignore::walk::WalkBuilder::get_or_set_current_dir` (calls)
- + `ignore::walk::WalkBuilder::build_matchers` -> `ignore::walk::WalkBuilder::build_ignore` (calls)
- + `ignore::walk::WalkBuilder::build_parallel` -> `ignore::walk::WalkBuilder::build_ignore` (calls)
- - `ignore::walk::WalkBuilder::build` -> `ignore::walk::WalkBuilder::get_or_set_current_dir` (calls)
- - `ignore::walk::WalkBuilder::build_parallel` -> `ignore::walk::WalkBuilder::get_or_set_current_dir` (calls)


</details>

<details><summary>Structure diagram</summary>

**modules view**
```mermaid
flowchart LR
    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px
    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4
    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px
    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af
    n0["globset"]:::context
    n1["globset::glob"]:::context
    n2["ignore"]:::context
    n3["ignore::default_types"]:::context
    n4["ignore::dir"]:::context
    n5["ignore::gitignore"]:::context
    n6["ignore::incremental"]:::added
    n7["ignore::overrides"]:::context
    n8["ignore::pathutil"]:::context
    n9["ignore::types"]:::context
    n10["ignore::walk"]:::context
    n0 --> n1
    n1 --> n0
    n2 -->|+| n6
    n2 --> n10
    n4 --> n2
    n4 --> n5
    n4 --> n7
    n4 --> n8
    n4 --> n9
    n4 --> n10
    n5 --> n0
    n5 --> n1
    n5 --> n2
    n5 --> n8
    n6 -->|+| n2
    n6 -->|+| n4
    n6 -->|+| n8
    n7 --> n2
    n7 --> n5
    n8 --> n10
    n9 --> n0
    n9 --> n1
    n9 --> n2
    n9 --> n3
    n9 --> n8
    n10 --> n2
    n10 --> n4
    n10 --> n5
    n10 -->|+| n6
    n10 --> n7
    n10 --> n9

note: no .csd.toml - running with defaults (module view, cycles warned). Run `csd init`, then declare layers to fail the build on forbidden dependencies.
```

</details>
