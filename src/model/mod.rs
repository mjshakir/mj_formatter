macro_rules! impl_str_partial_eq {
    ($type:ty) => {
        impl PartialEq<&str> for $type {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }
        impl PartialEq<$type> for &str {
            fn eq(&self, other: &$type) -> bool {
                *self == other.as_str()
            }
        }
    };
}

pub mod edit;
pub mod file_result;
pub mod pass_result;
pub mod policy_context;
pub mod exec_trace;
pub mod policy_result;
pub mod project_query;
pub mod report_record;
pub mod retry_strategy;
pub mod run_summary;
pub mod context_query;
pub mod rename_plan;
pub mod violation;
