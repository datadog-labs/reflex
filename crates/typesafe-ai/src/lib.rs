//! Standalone typed TypeSafe AI client. No dependency on Reflex.
mod client;
mod questions;
mod telemetry;
pub use client::*;
pub use questions::*;
#[doc(hidden)]
pub mod __private {
    pub use serde_json;
}

/// Build a heterogeneous set of named questions with named, typed answers.
///
/// ```
/// use typesafe_ai::{choice, noul, questions, SystemOneTask};
/// let task = SystemOneTask::builder().model("jev-latest").questions(questions! {
///     action: choice("Choose an action", [("admit", "Accept"), ("defer", "Wait")]),
///     urgent: noul("Is this urgent?"),
/// }).build().unwrap();
/// ```
#[macro_export]
macro_rules! questions {
    ($($name:ident: $question:expr),+ $(,)?) => {{
        #[allow(non_camel_case_types)]
        #[derive(Clone)]
        struct Questions<$($name),+> { $($name: $name),+ }
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone)]
        struct Answers<$($name),+> { $(pub $name: $name),+ }
        #[allow(non_camel_case_types)]
        impl<$($name: $crate::Question),+> $crate::QuestionSet for Questions<$($name),+> {
            type Answers = Answers<$($name::Answer),+>;
            fn encode(&self) -> Result<$crate::__private::serde_json::Value, $crate::Error> {
                let mut map = $crate::__private::serde_json::Map::new();
                $(map.insert(stringify!($name).into(), self.$name.encode()?);)+
                Ok(map.into())
            }
            fn decode(&self, value: &$crate::__private::serde_json::Value) -> Result<Self::Answers, $crate::Error> {
                let map = value.as_object().ok_or_else(|| $crate::Error::invalid_response("answers must be an object"))?;
                let expected = [$(stringify!($name)),+];
                if map.len() != expected.len() || expected.iter().any(|key| !map.contains_key(*key)) {
                    return Err($crate::Error::invalid_response("answer names do not match questions"));
                }
                Ok(Answers { $($name: self.$name.decode(&map[stringify!($name)])?),+ })
            }
        }
        Questions { $($name: $question),+ }
    }};
}
