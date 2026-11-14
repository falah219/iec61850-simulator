//! FFI bindings untuk libIEC61850 (hasil bindgen di build.rs), pola sama dengan fep-61850.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
