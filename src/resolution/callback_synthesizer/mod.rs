//! Callback / observer edge synthesis.

mod arkui;
mod c_fnptr;
mod celery;
mod channels;
mod conventions;
mod cross_platform;
mod edges;
mod erlang;
mod fabric;
mod flutter;
mod gin;
mod go_grpc;
mod go_interfaces;
mod goframe;
mod jsx;
mod kotlin;
mod laravel_events;
mod mediatr;
mod mybatis;
mod nix;
mod object_registry;
mod ordered;
mod overrides;
mod react;
mod rn;
mod sidekiq;
mod source;
mod spring;
mod state_stores;
mod synthesizer;
mod vue;

#[cfg(test)]
mod tests;

pub use synthesizer::synthesize_callback_edges;
