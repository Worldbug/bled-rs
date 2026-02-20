use anyhow::Result;
use tokio::select;

use crate::desk_controller::DeskEvent;

pub(crate) async fn interactive_mode(
    set_target_height: tokio::sync::mpsc::Sender<f32>,
) -> Result<()> {
    loop {
        let target = inquire::Text::new("Укажите высоту:").prompt()?;
        let target: f32 = target.parse()?;
        set_target_height.send(target).await?;
    }
}

pub(crate) async fn logger(
    mut desk_events: tokio::sync::broadcast::Receiver<DeskEvent>,
) -> Result<()> {
    loop {
        select! {
            Ok(event) = desk_events.recv() => {
                println!("{:#?}", event);
            }
        }
    }
}
