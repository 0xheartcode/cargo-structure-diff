<!-- csd-structure-gate -->
## Structure gate: FAILED

### Findings

- [deny] layering: crate::domain::order (domain) must not depend on crate::infra::db (infra)

<details><summary>Changelog</summary>

# Structure changelog vs 2c33842567031e7846ec11d30d7a96af524888c9

## crate::domain::order

### Modified
- struct `Order` (+db)

## Dependencies

- + `crate::domain::order` -> `crate::infra::db` (uses)


</details>

<details><summary>Structure diagram</summary>

**modules view**
```mermaid
flowchart LR
    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px
    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4
    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px
    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af
    n0["crate::domain::order"]:::context
    n1["crate::infra::db"]:::context
    n0 -->|+| n1

```

</details>
