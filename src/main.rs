pub(crate) mod cli;
pub(crate) mod desk_controller;

use anyhow::Result;
use std::sync::Arc;

use btleplug::api::{Manager as _, Peripheral};
use btleplug::platform::Manager;

use cli::{interactive_mode, logger};
use desk_controller::{device_finder, start_controller, start_read_thread, start_write_thread};

#[tokio::main]
async fn main() -> Result<()> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let central = adapters
        .into_iter()
        .next()
        .ok_or("Adapter not found")
        .unwrap();

    let desk = device_finder(central).await?;
    let desk = Arc::new(desk);

    desk.connect().await?;
    desk.discover_services().await?;

    let (s, desk_events) = tokio::sync::broadcast::channel(1);
    let (desk_commands, r) = tokio::sync::mpsc::channel(1);
    let (set_target_height, get_target_height) = tokio::sync::mpsc::channel(1);

    tokio::spawn(async move {
        let _ = interactive_mode(set_target_height).await;
    });

    let ui = desk_events.resubscribe();
    tokio::spawn(async move {
        let _ = logger(ui).await;
    });

    let _ = tokio::join!(
        start_read_thread(desk.clone(), s),
        start_write_thread(desk.clone(), r),
        start_controller(desk_events, desk_commands, get_target_height),
    );

    Ok(())
}
