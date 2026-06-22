//! Callback / observer edge synthesis.

mod channels;
mod edges;
mod fabric;
mod flutter;
mod gin;
mod go_grpc;
mod jsx;
mod mybatis;
mod ordered;
mod overrides;
mod react;
mod rn;
mod source;
mod synthesizer;
mod vue;

#[cfg(test)]
mod tests;

pub use synthesizer::synthesize_callback_edges;
