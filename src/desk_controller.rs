use anyhow::Result;
use btleplug::api::{Central, Peripheral, ScanFilter, WriteType::WithoutResponse};
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

const ERGOSTOL: Uuid = Uuid::from_u128(0x0000ff12_0000_1000_8000_00805f9b34fb);

const CHAR_WRITE: Uuid = Uuid::from_u128(0x0000ff01_0000_1000_8000_00805f9b34fb);
const CHAR_NOTIFY: Uuid = Uuid::from_u128(0x0000ff02_0000_1000_8000_00805f9b34fb);

const CMD_UP: &[u8] = &[0x02, 0x01, 0x00, 0x00, 0xAA, 0xAD];
const CMD_DOWN: &[u8] = &[0x01, 0x01, 0x00, 0x00, 0xAA, 0xE9];
const CMD_STOP: &[u8] = &[0x09, 0x01, 0x00, 0x00, 0xA8, 0x89];
const CMD_PULL: &[u8] = &[0x08, 0x01, 0x00, 0x00, 0xA9, 0x75];

#[derive(Clone, Debug)]
pub(crate) enum DeskEvent {
    StartMoving,
    StartMovingUp,
    StartMovingDown,
    MovingEnd(f32),
    HeightMoving(f32),
    HeightStatic(f32),
}

#[derive(Clone, Debug)]
pub(crate) enum DeskCommand {
    MoveUp,
    MoveDown,
    Stop,
    GetHeight,
}

impl From<DeskCommand> for &'static [u8] {
    fn from(cmd: DeskCommand) -> Self {
        match cmd {
            DeskCommand::MoveUp => CMD_UP,
            DeskCommand::MoveDown => CMD_DOWN,
            DeskCommand::Stop => CMD_STOP,
            DeskCommand::GetHeight => CMD_PULL,
        }
    }
}

fn get_height(p1: u8, p2: u8) -> f32 {
    let raw = ((p1 as u16) << 8) | (p2 as u16);
    let height = (raw as f32 / 43.22) + 24.16;
    return height;
}

pub(crate) async fn device_finder(
    central: btleplug::platform::Adapter,
) -> Result<btleplug::platform::Peripheral> {
    let filter = ScanFilter {
        services: vec![ERGOSTOL],
    };

    central.start_scan(filter).await?;

    loop {
        let peripherals = central.peripherals().await?;

        if let Some(desk) = peripherals.into_iter().next() {
            central.stop_scan().await?;
            return Ok(desk);
        }
    }
}

pub(crate) async fn start_read_thread(
    desk: Arc<btleplug::platform::Peripheral>,
    notify: broadcast::Sender<DeskEvent>,
) -> Result<()> {
    let notify_char = desk
        .characteristics()
        .iter()
        .find(|c| c.uuid == CHAR_NOTIFY)
        .ok_or("Notify char not found")
        .unwrap()
        .clone();

    desk.subscribe(&notify_char).await?;
    let mut stream = desk.notifications().await?;

    while let Some(data) = stream.next().await {
        let (p1, p2, p3, p4) = (data.value[0], data.value[1], data.value[2], data.value[3]);

        match p1 {
            0x0B => {
                let _ = notify.send(DeskEvent::StartMoving);
            }
            0x08 => {
                let h = get_height(p3, p4);

                match p2 {
                    0x01 => {
                        let _ = notify.send(DeskEvent::HeightMoving(h));
                    }
                    0x06 => {
                        let _ = notify.send(DeskEvent::HeightStatic(h));
                    }
                    _ => {}
                };
            }
            0x09 => {
                let h = get_height(p3, p4);
                let _ = notify.send(DeskEvent::MovingEnd(h));
            }
            0x02 => {
                let _ = notify.send(DeskEvent::StartMovingUp);
            }
            0x01 => {
                let _ = notify.send(DeskEvent::StartMovingDown);
            }
            _ => {}
        }
    }

    Ok(())
}

pub(crate) async fn start_write_thread(
    desk: Arc<btleplug::platform::Peripheral>,
    mut cmd: mpsc::Receiver<DeskCommand>,
) -> Result<()> {
    let notify_char = &desk
        .characteristics()
        .iter()
        .find(|c| c.uuid == CHAR_WRITE)
        .ok_or("Notify char not found")
        .unwrap()
        .clone();

    while let Some(cmd) = cmd.recv().await {
        desk.write(notify_char, cmd.into(), WithoutResponse).await?;
    }
    Ok(())
}

pub(crate) async fn start_controller(
    mut desk_events: broadcast::Receiver<DeskEvent>,
    desk_commands: mpsc::Sender<DeskCommand>,
    mut get_target_height: mpsc::Receiver<f32>,
) -> Result<()> {
    desk_commands.send(DeskCommand::GetHeight).await?;

    let mut current_height: f32 = 0.0;
    let mut target_height: f32 = 0.0;
    let mut move_up: Option<bool> = None;
    let is_moving = Arc::new(AtomicBool::new(false));

    loop {
        if is_moving.load(Ordering::SeqCst) {
            let _ = desk_commands.send(DeskCommand::GetHeight).await;
        }

        tokio::select! {
            Some(target) = get_target_height.recv() => {

                if is_moving.load(Ordering::SeqCst) {
                    desk_commands.send(DeskCommand::Stop).await?;
                }

                target_height = target;
                if current_height <= target_height {
                    move_up = Some(true);
                    desk_commands.send(DeskCommand::MoveUp).await?;
                } else if current_height >= target_height {
                    move_up = Some(false);
                    desk_commands.send(DeskCommand::MoveDown).await?;
                }
            }

            Ok(event) = desk_events.recv() => {
                match event {
                    DeskEvent::StartMoving => {
                        let is_moving = is_moving.clone();
                        is_moving.store(true, Ordering::SeqCst);
                    }

                    DeskEvent::HeightMoving(h) => {
                        current_height = h;
                        is_moving.store(true, Ordering::SeqCst);

                        match move_up {
                            Some(true) => {
                                if current_height > target_height {
                                    desk_commands.send(DeskCommand::Stop).await?;
                                };
                            },
                            Some(false) => {
                                if current_height < target_height {
                                    desk_commands.send(DeskCommand::Stop).await?;
                                };
                            },
                            _ => {},
                        };
                    }

                    DeskEvent::HeightStatic(h) => {
                        current_height = h;
                        is_moving.store(false, Ordering::SeqCst);
                    }

                    DeskEvent::StartMovingUp => {
                        is_moving.store(true, Ordering::SeqCst);
                    }

                    DeskEvent::StartMovingDown => {
                        is_moving.store(true, Ordering::SeqCst);
                    }

                    DeskEvent::MovingEnd(h) => {
                        current_height = h;
                        is_moving.store(false, Ordering::SeqCst);
                        move_up = None;
                        target_height = 0.0;
                    }
                }
            }
        }
    }
}
