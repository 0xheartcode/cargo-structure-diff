<!-- csd-structure-gate -->
## Structure gate: passed

<details><summary>Changelog</summary>

# Structure changelog vs d99ac34406f8

## grep_index (modified)

## grep_index::index (added)

### Added
- enum `Handle`
- struct `Index`
- struct `IndexBuilder`
- struct `IndexDiscovery`

## grep_index::index::Handle

### Added
- variant `ReadOnly`
- variant `ReadWrite`
- fn `begin_read`
- fn `fmt`
- fn `read_write`
- fn `read_write_mut`

## grep_index::index::Index

### Added
- fn `builder`
- fn `exists`
- fn `path`

## grep_index::index::IndexBuilder

### Added
- fn `build`
- fn `build_create`
- fn `build_open`
- fn `create`
- fn `new`
- fn `write`

## grep_index::index::IndexDiscovery

### Added
- fn `create`
- fn `cwd`
- fn `dir_name`
- fn `discover`
- fn `env_name`
- fn `new`

## Dependencies

- + `grep_index::index::Index` -> `grep_index::index::Handle` (associates)
- + `grep_index::index::IndexBuilder::build` -> `grep_index::index::IndexBuilder::build_create` (calls)
- + `grep_index::index::IndexBuilder::build` -> `grep_index::index::IndexBuilder::build_open` (calls)
- + `grep_index::index::IndexDiscovery` -> `grep_index::index::IndexBuilder` (associates)


</details>

<details><summary>Structure diagram</summary>

**modules view**
```mermaid
flowchart LR
    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px
    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4
    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px
    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af
    n0["grep_index"]:::changed
    n1["grep_index::index"]:::added

note: no .csd.toml - running with defaults (module view, cycles warned). Run `csd init`, then declare layers to fail the build on forbidden dependencies.
```

</details>
