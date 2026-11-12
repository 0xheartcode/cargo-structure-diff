<!-- csd-structure-gate -->
## Structure gate: passed

### Findings

- [warn] cycles: module cycle: axum -> axum::boxed -> axum::error_handling -> axum::extension -> axum::extract -> axum::extract::connect_info -> axum::extract::matched_path -> axum::extract::multipart -> axum::extract::nested_path -> axum::extract::original_uri -> axum::extract::path -> axum::extract::path::inner_path -> axum::extract::query -> axum::extract::raw_form -> axum::extract::raw_query -> axum::extract::rejection -> axum::extract::ws -> axum::form -> axum::handler -> axum::handler::service -> axum::json -> axum::response -> axum::routing -> axum::routing::method_routing -> axum::routing::not_found -> axum::routing::path_router -> axum::routing::route -> axum::service_ext

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

**modules view**
```mermaid
flowchart LR
    classDef added fill:#f0fdf4,stroke:#22c55e,stroke-width:2px
    classDef removed fill:#fef2f2,stroke:#ef4444,stroke-width:2px,stroke-dasharray:4 4
    classDef changed fill:#fffbeb,stroke:#f59e0b,stroke-width:2px
    classDef context fill:#ffffff,stroke:#d1d5db,color:#9ca3af
    n0["auto_reload"]:::context
    n1["axum"]:::context
    n2["axum::body"]:::context
    n3["axum::boxed"]:::context
    n4["axum::error_handling"]:::context
    n5["axum::extension"]:::context
    n6["axum::extract"]:::context
    n7["axum::extract::connect_info"]:::context
    n8["axum::extract::matched_path"]:::context
    n9["axum::extract::multipart"]:::context
    n10["axum::extract::nested_path"]:::context
    n11["axum::extract::original_uri"]:::context
    n12["axum::extract::path"]:::context
    n13["axum::extract::path::de"]:::context
    n14["axum::extract::path::inner_path"]:::added
    n15["axum::extract::query"]:::context
    n16["axum::extract::raw_form"]:::context
    n17["axum::extract::raw_query"]:::context
    n18["axum::extract::rejection"]:::context
    n19["axum::extract::state"]:::context
    n20["axum::extract::ws"]:::context
    n21["axum::extract::ws::rejection"]:::context
    n22["axum::form"]:::context
    n23["axum::handler"]:::context
    n24["axum::handler::future"]:::context
    n25["axum::handler::service"]:::context
    n26["axum::json"]:::context
    n27["axum::middleware"]:::context
    n28["axum::middleware::from_extractor"]:::context
    n29["axum::middleware::from_fn"]:::context
    n30["axum::middleware::future"]:::context
    n31["axum::middleware::map_request"]:::context
    n32["axum::middleware::map_response"]:::context
    n33["axum::middleware::response_axum_body"]:::context
    n34["axum::response"]:::context
    n35["axum::response::redirect"]:::context
    n36["axum::response::sse"]:::context
    n37["axum::routing"]:::context
    n38["axum::routing::future"]:::context
    n39["axum::routing::into_make_service"]:::context
    n40["axum::routing::method_filter"]:::context
    n41["axum::routing::method_routing"]:::context
    n42["axum::routing::not_found"]:::context
    n43["axum::routing::path_router"]:::context
    n44["axum::routing::route"]:::context
    n45["axum::routing::strip_prefix"]:::context
    n46["axum::routing::url_params"]:::context
    n47["axum::serve"]:::context
    n48["axum::service_ext"]:::context
    n49["axum::util"]:::context
    n50["axum_core"]:::context
    n51["axum_core::body"]:::context
    n52["axum_core::error"]:::context
    n53["axum_core::ext_traits::request"]:::context
    n54["axum_core::ext_traits::request_parts"]:::context
    n55["axum_core::extract"]:::context
    n56["axum_core::extract::default_body_limit"]:::context
    n57["axum_core::extract::default_body_limit::private"]:::context
    n58["axum_core::extract::from_ref"]:::context
    n59["axum_core::extract::option"]:::context
    n60["axum_core::extract::private"]:::context
    n61["axum_core::extract::rejection"]:::context
    n62["axum_core::extract::request_parts"]:::context
    n63["axum_core::extract::tuple"]:::context
    n64["axum_core::response"]:::context
    n65["axum_core::response::append_headers"]:::context
    n66["axum_core::response::into_response"]:::context
    n67["axum_core::response::into_response_parts"]:::context
    n68["axum_extra"]:::context
    n69["axum_extra::body::async_read_body"]:::context
    n70["axum_extra::either"]:::context
    n71["axum_extra::extract"]:::context
    n72["axum_extra::extract::cached"]:::context
    n73["axum_extra::extract::cookie"]:::context
    n74["axum_extra::extract::cookie::private"]:::context
    n75["axum_extra::extract::cookie::signed"]:::context
    n76["axum_extra::extract::form"]:::context
    n77["axum_extra::extract::json_deserializer"]:::context
    n78["axum_extra::extract::multipart"]:::context
    n79["axum_extra::extract::query"]:::context
    n80["axum_extra::extract::with_rejection"]:::context
    n81["axum_extra::handler"]:::context
    n82["axum_extra::handler::or"]:::context
    n83["axum_extra::json_lines"]:::context
    n84["axum_extra::protobuf"]:::context
    n85["axum_extra::response"]:::context
    n86["axum_extra::response::attachment"]:::context
    n87["axum_extra::response::erased_json"]:::context
    n88["axum_extra::response::error_response"]:::context
    n89["axum_extra::response::file_stream"]:::context
    n90["axum_extra::response::multiple"]:::context
    n91["axum_extra::routing"]:::context
    n92["axum_extra::routing::resource"]:::context
    n93["axum_extra::routing::typed"]:::context
    n94["axum_extra::typed_header"]:::context
    n95["axum_macros"]:::context
    n96["axum_macros::debug_handler"]:::context
    n97["example_anyhow_error_response"]:::context
    n98["example_chat"]:::context
    n99["example_compression"]:::context
    n100["example_consume_body_in_extractor_or_middleware"]:::context
    n101["example_cors"]:::context
    n102["example_customize_extractor_error"]:::context
    n103["example_customize_extractor_error::custom_extractor"]:::context
    n104["example_customize_extractor_error::derive_from_request"]:::context
    n105["example_customize_extractor_error::with_rejection"]:::context
    n106["example_customize_path_rejection"]:::context
    n107["example_dependency_injection"]:::context
    n108["example_diesel_async_postgres"]:::context
    n109["example_diesel_postgres"]:::context
    n110["example_error_handling"]:::context
    n111["example_form"]:::context
    n112["example_global_404_handler"]:::context
    n113["example_graceful_shutdown"]:::context
    n114["example_handle_head_request"]:::context
    n115["example_hello_world"]:::context
    n116["example_http_proxy"]:::context
    n117["example_jwt"]:::context
    n118["example_key_value_store"]:::context
    n119["example_low_level_native_tls"]:::context
    n120["example_low_level_openssl"]:::context
    n121["example_low_level_rustls"]:::context
    n122["example_mongodb"]:::context
    n123["example_multipart_form"]:::context
    n124["example_oauth"]:::context
    n125["example_parse_body_based_on_content_type"]:::context
    n126["example_print_request_response"]:::context
    n127["example_prometheus_metrics"]:::context
    n128["example_readme"]:::context
    n129["example_request_id"]:::context
    n130["example_reqwest_response"]:::context
    n131["example_reverse_proxy"]:::context
    n132["example_routes_and_handlers_close_together"]:::context
    n133["example_serve_with_hyper"]:::context
    n134["example_simple_router_wasm"]:::context
    n135["example_sqlx_postgres"]:::context
    n136["example_sse"]:::context
    n137["example_static_file_server"]:::context
    n138["example_stream_to_file"]:::context
    n139["example_templates"]:::context
    n140["example_templates_minijinja"]:::context
    n141["example_testing"]:::context
    n142["example_testing_websockets"]:::context
    n143["example_tls_graceful_shutdown"]:::context
    n144["example_tls_rustls"]:::context
    n145["example_todos"]:::context
    n146["example_tokio_postgres"]:::context
    n147["example_tokio_redis"]:::context
    n148["example_tracing_aka_logging"]:::context
    n149["example_unix_domain_socket::unix"]:::context
    n150["example_validator"]:::context
    n151["example_versioning"]:::context
    n152["example_websockets"]:::context
    n153["example_websockets_http2"]:::context
    n0 --> n34
    n0 --> n37
    n0 --> n41
    n1 --> n5
    n1 --> n22
    n1 --> n26
    n1 --> n37
    n1 --> n47
    n1 --> n48
    n1 --> n50
    n1 --> n52
    n1 --> n53
    n1 --> n54
    n1 --> n95
    n1 --> n96
    n2 --> n51
    n3 --> n6
    n3 --> n23
    n3 --> n37
    n3 --> n44
    n4 --> n6
    n4 --> n34
    n5 --> n34
    n5 --> n55
    n5 --> n59
    n5 --> n64
    n5 --> n66
    n5 --> n67
    n6 --> n5
    n6 --> n7
    n6 --> n8
    n6 --> n9
    n6 --> n10
    n6 --> n11
    n6 --> n12
    n6 -->|+| n14
    n6 --> n15
    n6 --> n16
    n6 --> n17
    n6 --> n19
    n6 --> n20
    n6 --> n22
    n6 --> n26
    n6 --> n55
    n6 --> n56
    n6 --> n58
    n6 --> n59
    n6 --> n95
    n7 --> n5
    n7 --> n6
    n8 --> n6
    n8 --> n37
    n8 --> n59
    n9 --> n2
    n9 --> n6
    n9 --> n50
    n9 --> n53
    n9 --> n59
    n9 --> n64
    n9 --> n66
    n10 --> n6
    n10 --> n18
    n10 --> n55
    n11 --> n5
    n11 --> n6
    n12 --> n6
    n12 --> n46
    n12 --> n49
    n12 --> n54
    n12 --> n59
    n12 --> n64
    n12 --> n66
    n13 --> n12
    n13 --> n49
    n14 -->|+| n6
    n14 -->|+| n12
    n14 -->|+| n18
    n14 -->|+| n46
    n14 -->|+| n49
    n14 -->|+| n54
    n14 -->|+| n59
    n15 --> n6
    n16 --> n6
    n16 --> n18
    n16 --> n55
    n17 --> n6
    n18 --> n12
    n18 --> n50
    n19 --> n55
    n19 --> n58
    n20 --> n1
    n20 --> n2
    n20 --> n6
    n20 --> n34
    n20 --> n51
    n21 --> n50
    n22 --> n6
    n22 --> n16
    n22 --> n53
    n22 --> n64
    n22 --> n66
    n23 --> n6
    n23 --> n7
    n23 --> n25
    n23 --> n34
    n23 --> n39
    n24 --> n34
    n24 --> n55
    n25 --> n1
    n25 --> n2
    n25 --> n7
    n25 --> n23
    n25 --> n34
    n25 --> n39
    n26 --> n6
    n26 --> n59
    n26 --> n64
    n26 --> n66
    n27 --> n5
    n27 --> n28
    n27 --> n29
    n27 --> n31
    n27 --> n32
    n27 --> n33
    n28 --> n6
    n28 --> n34
    n29 --> n34
    n29 --> n49
    n29 --> n55
    n30 --> n28
    n30 --> n29
    n30 --> n31
    n30 --> n32
    n31 --> n1
    n31 --> n2
    n31 --> n34
    n31 --> n55
    n32 --> n34
    n32 --> n55
    n33 --> n51
    n33 --> n64
    n34 --> n5
    n34 --> n22
    n34 --> n26
    n34 --> n35
    n34 --> n64
    n34 --> n65
    n34 --> n66
    n34 --> n67
    n35 --> n64
    n35 --> n66
    n35 --> n67
    n36 --> n1
    n36 --> n2
    n36 --> n51
    n36 --> n64
    n36 --> n66
    n37 --> n2
    n37 --> n3
    n37 --> n7
    n37 --> n8
    n37 --> n23
    n37 --> n39
    n37 --> n40
    n37 --> n41
    n37 --> n42
    n37 --> n43
    n37 --> n44
    n37 --> n49
    n37 --> n55
    n37 --> n64
    n37 --> n66
    n38 --> n39
    n38 --> n44
    n41 --> n2
    n41 --> n3
    n41 --> n4
    n41 --> n7
    n41 --> n23
    n41 --> n34
    n41 --> n37
    n41 --> n39
    n41 --> n40
    n41 --> n44
    n41 --> n50
    n41 --> n55
    n41 --> n66
    n42 --> n34
    n42 --> n66
    n43 --> n6
    n43 --> n10
    n43 --> n23
    n43 --> n37
    n43 --> n41
    n43 --> n44
    n43 --> n45
    n43 --> n46
    n43 --> n66
    n44 --> n2
    n44 --> n34
    n44 --> n49
    n44 --> n55
    n44 --> n66
    n45 -->|+| n46
    n46 --> n49
    n47 --> n51
    n47 --> n55
    n47 --> n64
    n48 --> n4
    n48 --> n7
    n48 --> n39
    n49 --> n64
    n49 --> n66
    n50 --> n52
    n50 --> n53
    n50 --> n54
    n51 --> n50
    n51 --> n52
    n52 --> n50
    n53 --> n51
    n53 --> n55
    n53 --> n56
    n54 --> n55
    n55 --> n51
    n55 --> n56
    n55 --> n58
    n55 --> n59
    n55 --> n66
    n56 --> n57
    n57 --> n56
    n59 --> n55
    n59 --> n60
    n59 --> n66
    n61 --> n50
    n61 --> n52
    n62 --> n51
    n62 --> n53
    n62 --> n55
    n63 --> n55
    n63 --> n64
    n63 --> n66
    n64 --> n51
    n64 --> n65
    n64 --> n66
    n64 --> n67
    n65 --> n64
    n65 --> n66
    n65 --> n67
    n66 --> n50
    n66 --> n51
    n66 --> n64
    n66 --> n67
    n67 --> n64
    n67 --> n66
    n69 --> n51
    n69 --> n52
    n69 --> n64
    n69 --> n66
    n70 --> n55
    n70 --> n64
    n70 --> n66
    n71 --> n72
    n71 --> n73
    n71 --> n74
    n71 --> n75
    n71 --> n76
    n71 --> n77
    n71 --> n78
    n71 --> n79
    n71 --> n80
    n71 --> n83
    n71 --> n94
    n72 --> n6
    n73 --> n55
    n73 --> n64
    n73 --> n66
    n73 --> n67
    n73 --> n74
    n73 --> n75
    n74 --> n55
    n74 --> n58
    n74 --> n64
    n74 --> n66
    n74 --> n67
    n74 --> n73
    n75 --> n55
    n75 --> n58
    n75 --> n64
    n75 --> n66
    n75 --> n67
    n75 --> n73
    n76 --> n1
    n76 --> n6
    n76 --> n16
    n76 --> n18
    n76 --> n50
    n77 --> n50
    n77 --> n55
    n77 --> n61
    n78 --> n50
    n78 --> n51
    n78 --> n53
    n78 --> n55
    n78 --> n64
    n78 --> n66
    n79 --> n50
    n79 --> n55
    n80 --> n6
    n80 --> n34
    n80 --> n93
    n81 --> n2
    n81 --> n6
    n81 --> n23
    n81 --> n34
    n81 --> n82
    n82 --> n6
    n82 --> n23
    n82 --> n34
    n82 --> n70
    n82 --> n81
    n83 --> n50
    n83 --> n51
    n83 --> n53
    n83 --> n55
    n83 --> n64
    n83 --> n66
    n84 --> n50
    n84 --> n55
    n84 --> n61
    n84 --> n64
    n84 --> n66
    n85 --> n83
    n85 --> n94
    n86 --> n66
    n87 --> n64
    n87 --> n66
    n88 --> n64
    n88 --> n66
    n89 --> n50
    n89 --> n51
    n89 --> n64
    n89 --> n66
    n90 --> n64
    n90 --> n66
    n91 --> n6
    n91 --> n11
    n91 --> n34
    n91 --> n35
    n91 --> n37
    n91 --> n40
    n91 --> n41
    n91 --> n92
    n91 --> n93
    n91 --> n95
    n92 --> n23
    n92 --> n37
    n92 --> n40
    n92 --> n41
    n94 --> n55
    n94 --> n59
    n94 --> n64
    n94 --> n66
    n94 --> n67
    n97 --> n1
    n97 --> n34
    n97 --> n37
    n97 --> n41
    n98 --> n19
    n98 --> n20
    n98 --> n34
    n98 --> n37
    n98 --> n41
    n99 --> n26
    n99 --> n37
    n99 --> n41
    n100 --> n1
    n100 --> n2
    n100 --> n6
    n100 --> n27
    n100 --> n29
    n100 --> n34
    n100 --> n37
    n100 --> n41
    n101 --> n1
    n101 --> n26
    n101 --> n34
    n101 --> n37
    n101 --> n41
    n102 --> n37
    n102 --> n41
    n103 --> n1
    n103 --> n6
    n103 --> n8
    n103 --> n18
    n103 --> n34
    n104 --> n1
    n104 --> n6
    n104 --> n18
    n104 --> n34
    n105 --> n18
    n105 --> n26
    n105 --> n34
    n105 --> n80
    n106 --> n1
    n106 --> n6
    n106 --> n12
    n106 --> n18
    n106 --> n34
    n106 --> n37
    n106 --> n41
    n107 --> n1
    n107 --> n12
    n107 --> n19
    n107 --> n26
    n107 --> n37
    n107 --> n41
    n108 --> n1
    n108 --> n6
    n108 --> n19
    n108 --> n26
    n108 --> n37
    n108 --> n41
    n109 --> n1
    n109 --> n19
    n109 --> n26
    n109 --> n37
    n109 --> n41
    n110 --> n1
    n110 --> n6
    n110 --> n8
    n110 --> n18
    n110 --> n19
    n110 --> n29
    n110 --> n34
    n110 --> n37
    n110 --> n41
    n111 --> n22
    n111 --> n34
    n111 --> n37
    n111 --> n41
    n112 --> n1
    n112 --> n34
    n112 --> n37
    n112 --> n41
    n113 --> n1
    n113 --> n37
    n113 --> n41
    n114 --> n1
    n114 --> n34
    n114 --> n37
    n114 --> n41
    n115 --> n34
    n115 --> n37
    n115 --> n41
    n116 --> n1
    n116 --> n2
    n116 --> n6
    n116 --> n34
    n116 --> n37
    n116 --> n41
    n117 --> n1
    n117 --> n6
    n117 --> n26
    n117 --> n34
    n117 --> n37
    n117 --> n41
    n117 --> n68
    n118 --> n1
    n118 --> n2
    n118 --> n4
    n118 --> n6
    n118 --> n12
    n118 --> n19
    n118 --> n23
    n118 --> n34
    n118 --> n37
    n118 --> n41
    n119 --> n6
    n119 --> n37
    n119 --> n41
    n120 --> n1
    n120 --> n37
    n120 --> n41
    n121 --> n6
    n121 --> n37
    n121 --> n41
    n122 --> n1
    n122 --> n12
    n122 --> n19
    n122 --> n26
    n122 --> n37
    n122 --> n41
    n123 --> n6
    n123 --> n9
    n123 --> n34
    n123 --> n37
    n123 --> n41
    n124 --> n1
    n124 --> n6
    n124 --> n15
    n124 --> n19
    n124 --> n34
    n124 --> n35
    n124 --> n37
    n124 --> n41
    n124 --> n68
    n124 --> n94
    n125 --> n1
    n125 --> n6
    n125 --> n22
    n125 --> n26
    n125 --> n34
    n125 --> n37
    n125 --> n41
    n126 --> n1
    n126 --> n2
    n126 --> n6
    n126 --> n27
    n126 --> n29
    n126 --> n34
    n126 --> n37
    n126 --> n41
    n127 --> n6
    n127 --> n8
    n127 --> n27
    n127 --> n29
    n127 --> n34
    n127 --> n37
    n127 --> n41
    n128 --> n1
    n128 --> n26
    n128 --> n34
    n128 --> n37
    n128 --> n41
    n129 --> n1
    n129 --> n34
    n129 --> n37
    n129 --> n41
    n130 --> n1
    n130 --> n2
    n130 --> n19
    n130 --> n34
    n130 --> n37
    n130 --> n41
    n131 --> n1
    n131 --> n2
    n131 --> n6
    n131 --> n19
    n131 --> n34
    n131 --> n37
    n131 --> n41
    n132 --> n37
    n132 --> n41
    n133 --> n6
    n133 --> n7
    n133 --> n37
    n133 --> n41
    n134 --> n34
    n134 --> n37
    n134 --> n41
    n135 --> n1
    n135 --> n6
    n135 --> n19
    n135 --> n37
    n135 --> n41
    n136 --> n36
    n136 --> n37
    n136 --> n41
    n136 --> n68
    n137 --> n1
    n137 --> n6
    n137 --> n23
    n137 --> n37
    n137 --> n41
    n138 --> n1
    n138 --> n2
    n138 --> n6
    n138 --> n9
    n138 --> n12
    n138 --> n34
    n138 --> n35
    n138 --> n37
    n138 --> n41
    n139 --> n1
    n139 --> n6
    n139 --> n34
    n139 --> n37
    n139 --> n41
    n140 --> n1
    n140 --> n19
    n140 --> n34
    n140 --> n37
    n140 --> n41
    n141 --> n7
    n141 --> n26
    n141 --> n37
    n141 --> n41
    n142 --> n20
    n142 --> n34
    n142 --> n37
    n142 --> n41
    n143 --> n1
    n143 --> n23
    n143 --> n35
    n143 --> n37
    n143 --> n41
    n144 --> n1
    n144 --> n23
    n144 --> n35
    n144 --> n37
    n144 --> n41
    n145 --> n1
    n145 --> n4
    n145 --> n12
    n145 --> n15
    n145 --> n19
    n145 --> n26
    n145 --> n34
    n145 --> n37
    n145 --> n41
    n146 --> n1
    n146 --> n6
    n146 --> n19
    n146 --> n37
    n146 --> n41
    n147 --> n1
    n147 --> n6
    n147 --> n19
    n147 --> n37
    n147 --> n41
    n148 --> n1
    n148 --> n2
    n148 --> n8
    n148 --> n34
    n148 --> n37
    n148 --> n41
    n149 --> n1
    n149 --> n2
    n149 --> n7
    n149 --> n37
    n149 --> n41
    n149 --> n47
    n150 --> n1
    n150 --> n6
    n150 --> n18
    n150 --> n22
    n150 --> n34
    n150 --> n37
    n150 --> n41
    n151 --> n1
    n151 --> n6
    n151 --> n12
    n151 --> n34
    n151 --> n37
    n151 --> n41
    n152 --> n2
    n152 --> n7
    n152 --> n20
    n152 --> n34
    n152 --> n37
    n152 --> n41
    n152 --> n68
    n153 --> n1
    n153 --> n19
    n153 --> n20
    n153 --> n37
    n153 --> n41

note: no .csd.toml - running with defaults (module view, cycles warned). Run `csd init`, then declare layers to fail the build on forbidden dependencies.
```

</details>
