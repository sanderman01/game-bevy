//! Which ports the BRP server listens on, and failing loudly when they are taken.
//!
//! `bevy_remote` binds inside a detached IO task, so a port another game holds is an error
//! nobody sees: the game runs without a server and every call reaches the other game. This
//! plugin binds both ports itself at plugin build, holds them until just before Bevy's own bind,
//! and exits with [`PORT_IN_USE_EXIT_CODE`] when either is taken.

use std::net::{Ipv4Addr, TcpListener};

use bevy::{
    prelude::*,
    remote::http::{DEFAULT_PORT, HostPort},
    render::RenderApp,
};

/// The variable the launcher sets to give an instance its own pair of ports.
pub const PORT_VAR: &str = "ENAME_BRP_PORT";

/// The process exit code when a BRP port is already taken. The launcher reads it.
pub const PORT_IN_USE_EXIT_CODE: i32 = 3;

/// The main world's BRP port: [`PORT_VAR`], else Bevy's default. The render world's listener
/// takes the next one.
pub(crate) fn brp_port() -> u16 {
    let Some(value) = std::env::var_os(PORT_VAR) else {
        return DEFAULT_PORT;
    };
    match value.to_str().and_then(|text| text.parse::<u16>().ok()) {
        Some(port) if port < u16::MAX => port,
        _ => panic!("{PORT_VAR} must be a port number below 65535, not {value:?}"),
    }
}

/// Probes and holds `port` and `port + 1`, and points the render world's listener at the second.
///
/// Holding the listeners from build rather than probing once means a second game started at the
/// same moment fails here too; what is left is the one schedule step between dropping them and
/// Bevy's bind, which the launcher's pid check covers.
pub(crate) struct BrpPortPlugin {
    pub port: u16,
}

/// The probe listeners, dropped in `PreStartup` so Bevy's `Startup` bind can take the ports.
#[derive(Resource)]
struct HeldPorts(#[expect(dead_code, reason = "held only for their drop")] Vec<TcpListener>);

impl Plugin for BrpPortPlugin {
    fn build(&self, app: &mut App) {
        let render_port = self.port + 1;
        // std and async-io bind alike on each platform, so this fails exactly when Bevy's would.
        let held = [self.port, render_port]
            .into_iter()
            .map(|port| {
                TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap_or_else(|_| {
                    let message = format!(
                        "BRP port {port} is in use: another game is running. Stop it or set \
                         {PORT_VAR}."
                    );
                    error!("{message}");
                    std::process::exit(PORT_IN_USE_EXIT_CODE)
                })
            })
            .collect();

        app.insert_resource(HeldPorts(held))
            .add_systems(PreStartup, release_ports);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(HostPort(render_port));
        }
    }
}

fn release_ports(mut commands: Commands) {
    commands.remove_resource::<HeldPorts>();
}
