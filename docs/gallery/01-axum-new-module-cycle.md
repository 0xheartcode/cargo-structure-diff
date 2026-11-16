<!-- csd-structure-gate -->
## Structure gate: passed

<details><summary>Changelog</summary>

# Structure changelog vs 7c3c67879dc5

## axum::extract::path

### Added
- fn `get_params`
- fn `serialize_path_params`

### Modified
- struct `Path`

## axum::extract::path::ErrorKind

### Added
- fn `is_zero_params`

## axum::extract::path::Path

### Modified
- fn `from_request_parts`

## axum::extract::path::inner_path (added)

### Added
- struct `InnerPath`
- fn `get_inner_params`

## axum::extract::path::inner_path::InnerPath

### Added
- fn `from_request_parts`

## axum::routing::strip_prefix

### Added
- fn `count_captures`

### Modified
- struct `StripPrefix` (+captures_count)

## axum::routing::strip_prefix::StripPrefix

### Modified
- fn `call`

## axum::routing::url_params

### Added
- fn `advance_inner_start`

### Modified
- enum `UrlParams` (~Params)
- fn `insert_url_params`

## Dependencies

- + `axum::extract` -> `axum::extract::path::inner_path` (uses)
- + `axum::extract::path::Path::from_request_parts` -> `axum::extract::path::get_params` (calls)
- + `axum::extract::path::inner_path` -> `axum::extract` (uses)
- + `axum::extract::path::inner_path` -> `axum::extract::path` (uses)
- + `axum::extract::path::inner_path` -> `axum::extract::rejection` (uses)
- + `axum::extract::path::inner_path` -> `axum::routing::url_params` (uses)
- + `axum::extract::path::inner_path` -> `axum::util` (uses)
- + `axum::extract::path::inner_path` -> `axum_core::ext_traits::request_parts` (uses)
- + `axum::extract::path::inner_path` -> `axum_core::extract::option` (uses)
- + `axum::extract::path::inner_path::InnerPath::from_request_parts` -> `axum::extract::path::inner_path::get_inner_params` (calls)
- + `axum::routing::strip_prefix` -> `axum::routing::url_params` (uses)
- + `axum::routing::strip_prefix::StripPrefix::layer` -> `axum::routing::strip_prefix::count_captures` (calls)
- + `axum::routing::strip_prefix::count_captures` -> `axum::routing::strip_prefix::segments` (calls)


</details>

<details><summary>Structure diagram</summary>

_Structure diagram omitted: 667 edges exceeds GitHub's Mermaid render limit (500). Run `csd diff --base 7c3c67879dc5 --show --format svg` for the full graph._

</details>
