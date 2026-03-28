#[allow(non_upper_case_globals, dead_code)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/ts_cpp_symbols.rs"));
}

pub use generated::*;
