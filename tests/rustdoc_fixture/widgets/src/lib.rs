mod parts;

pub use parts::gear::Gear;

pub mod prelude {
    pub use crate::Gear as Cog;
    pub use widgets_core::Named;
}

pub mod task {
    pub use widgets_core::task::*;
}
