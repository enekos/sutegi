#![forbid(unsafe_code)]

mod decode;
mod impls;
mod registry;
pub mod schema;
pub mod snapshot;
pub mod ts;
mod ty;

pub use decode::{decode, decode_lenient, decode_with, Cx};
pub use registry::{ChannelSpec, Kind, Message, Procedure, Registry};
pub use sutegi_json::Json;
pub use sutegi_validate::ValidationErrors;
pub use ty::{Def, Field, Format, Rules, Shape, Type, Variant, VariantShape};

pub trait Contract: Sized {
    fn describe(reg: &mut Registry) -> Type;

    fn decode(value: &Json, cx: &mut Cx) -> Option<Self>;

    fn encode(&self) -> Json;

    fn missing() -> Option<Self> {
        None
    }
}

pub fn type_of<T: Contract>() -> (Registry, Type) {
    let mut reg = Registry::new();
    let ty = T::describe(&mut reg);
    (reg, ty)
}
