use anyhow::{Context, Result};
use evdev::{
    UinputAbsSetup,
    uinput::VirtualDeviceBuilder,
    AbsInfo,
    InputId,
    Key,
    AbsoluteAxisType,
    InputEvent,
    Device,
    InputEventKind,
    EventType,
};
use std::{
    thread,
    sync::mpsc,
    time::Duration,
};
use libdogd::{log_debug, log_info};

static MAX_OUT_ANALOG: i32 = 32767;
static MIN_OUT_ANALOG: i32 = -32768;

static MIN_OUT_HAT: i32 = -1;
static MAX_OUT_HAT: i32 = 1;

static MIN_OUT_TRIG: i32 = 0;
static MAX_OUT_TRIG: i32 = 255;

#[inline]
fn has_key(dev: &Device, key: evdev::Key) -> bool {
    dev.supported_keys().map_or(false, |keys| keys.contains(key))
}

fn generic_dac(ev: &mut InputEvent, _: mpsc::Sender<InputEvent>) {
    let InputEventKind::Key(key) = ev.kind() else { return };
    let type_value = match key {
        Key::BTN_DPAD_UP    => (AbsoluteAxisType::ABS_HAT0Y.0, if ev.value() == 0 { 0 } else { -1 }),
        Key::BTN_DPAD_DOWN  => (AbsoluteAxisType::ABS_HAT0Y.0, if ev.value() == 0 { 0 } else {  1 }),
        Key::BTN_DPAD_LEFT  => (AbsoluteAxisType::ABS_HAT0X.0, if ev.value() == 0 { 0 } else { -1 }),
        Key::BTN_DPAD_RIGHT => (AbsoluteAxisType::ABS_HAT0X.0, if ev.value() == 0 { 0 } else {  1 }),
        
        Key::BTN_TL2 => (AbsoluteAxisType::ABS_Z.0, if ev.value() == 0 { MIN_OUT_TRIG } else { MAX_OUT_TRIG }),
        Key::BTN_TR2 => (AbsoluteAxisType::ABS_RZ.0, if ev.value() == 0 { MIN_OUT_TRIG } else { MAX_OUT_TRIG }),
        _ => return,
    };
    *ev = InputEvent::new(EventType::ABSOLUTE, type_value.0, type_value.1);
}

fn rg351m(ev: &mut InputEvent, _: mpsc::Sender<InputEvent>) {
    let InputEventKind::Key(key) = ev.kind() else { return };
    // yes this is for real. maybe the engineers were drunk, *shrugs*
    let new_ev = match key {
        // abxy
        Key::BTN_EAST       => InputEvent::new(EventType::KEY, Key::BTN_SOUTH.0, ev.value()),
        Key::BTN_SOUTH      => InputEvent::new(EventType::KEY, Key::BTN_EAST.0, ev.value()),
        Key::BTN_NORTH      => InputEvent::new(EventType::KEY, Key::BTN_WEST.0, ev.value()),
        Key::BTN_C          => InputEvent::new(EventType::KEY, Key::BTN_NORTH.0, ev.value()),
        // thumb buttons
        Key::BTN_TL2        => InputEvent::new(EventType::KEY, Key::BTN_THUMBL.0, ev.value()),
        Key::BTN_TR2        => InputEvent::new(EventType::KEY, Key::BTN_THUMBR.0, ev.value()),
        // shoulders
        Key::BTN_WEST       => InputEvent::new(EventType::KEY, Key::BTN_TL.0, ev.value()),
        Key::BTN_Z          => InputEvent::new(EventType::KEY, Key::BTN_TR.0, ev.value()),
        // triggers
        Key::BTN_SELECT     => InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Z.0, ev.value() * MAX_OUT_TRIG),
        Key::BTN_START      => InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_RZ.0, ev.value() * MAX_OUT_TRIG),
        // select start
        Key::BTN_TR         => InputEvent::new(EventType::KEY, Key::BTN_SELECT.0, ev.value()),
        Key::BTN_TL         => InputEvent::new(EventType::KEY, Key::BTN_START.0, ev.value()),
        _ => return,
    };
    *ev = new_ev;
}

// TODO: multiple remap quirks
fn get_remap_fn(dev: &mut Device) -> Option<fn(&mut InputEvent, mpsc::Sender<InputEvent>)> {
    let inputid = dev.input_id();
    if inputid.vendor() == 0x1209 && inputid.product() == 0x3100 {
        log_info("Applying rg351m quirk");
        return Some(rg351m);
    }
    if has_key(&dev, Key::BTN_DPAD_LEFT) {
        log_info("Applying generic_dac quirk");
        return Some(generic_dac);
    }
    None
}

fn input_handler(tx: mpsc::Sender<InputEvent>, mut dev: Device) -> Result<()> {
    let mut useful = false;

    // gamepads
    if has_key(&dev, Key::BTN_SOUTH) {
        useful = true;
    }

    // touchscreens
    if has_key(&dev, Key::BTN_TOUCH) {
        useful = false;
    }

    // rinputer
    if dev.input_id().version() == 0x2137 {
        useful = false;
    }

    // steam input, note the space
    if dev.name().unwrap_or("Microsoft X-Box 360 pad ").starts_with("Microsoft X-Box 360 pad ") {
        useful = false;
    }

    if !useful {
        return Ok(());
    }

    match dev.grab() {
        Ok(()) => log_debug(format!("Device {} deemed useful", dev.name().unwrap_or("<invalid name>"))),
        Err(_) => return Ok(()), // fail silently in case someone else grabbed it before us
    };


    let mut abs_minimums: [i32; 6] = [0; 6];
    let mut abs_maximums: [i32; 6] = [0; 6];

    if let Ok(absinfo) = dev.get_abs_state() {
        for axis in 0..6 {
            abs_minimums[axis] = absinfo[axis].minimum;
            abs_maximums[axis] = absinfo[axis].maximum;
        }
    }

    let abs_multipliers_min = abs_minimums.into_iter()
        .enumerate()
        .map(|(i, v)| {
            let cmp_against = if i == AbsoluteAxisType::ABS_Z.0 as usize || i == AbsoluteAxisType::ABS_RZ.0 as usize {
                MIN_OUT_TRIG
            } else {
                MIN_OUT_ANALOG
            };
            if (v - cmp_against).abs() < 100 {
                1
            } else {
                cmp_against / v
            }
        })
        .collect::<Vec<i32>>();

    let abs_multipliers_max = abs_maximums.into_iter()
        .enumerate()
        .map(|(i, v)| {
            let cmp_against = if i == AbsoluteAxisType::ABS_Z.0 as usize || i == AbsoluteAxisType::ABS_RZ.0 as usize {
                MAX_OUT_TRIG
            } else {
                MAX_OUT_ANALOG
            };
            if (v - cmp_against).abs() < 100 {
                1
            } else {
                cmp_against / v
            }
        })
        .collect::<Vec<i32>>();

    let remap_fn = get_remap_fn(&mut dev);

    loop {
        for mut ev in dev.fetch_events()? {
            match ev.kind() {
                InputEventKind::AbsAxis(axis) => {
                    let val = match axis {
                        AbsoluteAxisType::ABS_HAT0Y => ev.value(), // assuming it's always between -1
                        AbsoluteAxisType::ABS_HAT0X => ev.value(), // and 1
                        _ => if ev.value() < 0 {
                            ev.value() * abs_multipliers_min[axis.0 as usize]
                        } else {
                            ev.value() * abs_multipliers_max[axis.0 as usize]
                        },
                    };
                    tx.send(InputEvent::new(ev.event_type(), ev.code(), val))?;
                }
                InputEventKind::Key(_) => {
                    if let Some(actual_remap_fn) = remap_fn {
                        actual_remap_fn(&mut ev, tx.clone());
                    }
                    tx.send(ev)?;
                },
                _ => (),
            }
        }
    }
}

fn indev_watcher(tx: mpsc::Sender<InputEvent>) {
    loop {
        for device in evdev::enumerate() {
            let new_tx = tx.clone();
            thread::spawn(move || input_handler(new_tx, device.1));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn main() -> Result<()> {
    let mut keys = evdev::AttributeSet::<Key>::new();
    keys.insert(Key::BTN_SOUTH);
    keys.insert(Key::BTN_EAST);
    keys.insert(Key::BTN_NORTH);
    keys.insert(Key::BTN_WEST);
    keys.insert(Key::BTN_TL);
    keys.insert(Key::BTN_TR);
    keys.insert(Key::BTN_SELECT);
    keys.insert(Key::BTN_START);
    keys.insert(Key::BTN_MODE);
    keys.insert(Key::BTN_THUMBL);
    keys.insert(Key::BTN_THUMBR);

    let input_id = InputId::new(evdev::BusType::BUS_USB, 0x045e, 0x028e, 0x2137);

    let abs_analogs = AbsInfo::new(0, MIN_OUT_ANALOG, MAX_OUT_ANALOG, 16, 256, 0);
    let abs_x = UinputAbsSetup::new(AbsoluteAxisType::ABS_X, abs_analogs);
    let abs_y = UinputAbsSetup::new(AbsoluteAxisType::ABS_Y, abs_analogs);
    let abs_rx = UinputAbsSetup::new(AbsoluteAxisType::ABS_RX, abs_analogs);
    let abs_ry = UinputAbsSetup::new(AbsoluteAxisType::ABS_RY, abs_analogs);

    let abs_triggers = AbsInfo::new(0, MIN_OUT_TRIG, MAX_OUT_TRIG, 0, 0, 0);
    let abs_z = UinputAbsSetup::new(AbsoluteAxisType::ABS_Z, abs_triggers);
    let abs_rz = UinputAbsSetup::new(AbsoluteAxisType::ABS_RZ, abs_triggers);

    let abs_hat = AbsInfo::new(0, MIN_OUT_HAT, MAX_OUT_HAT, 0, 0, 0);
    let abs_hat_x = UinputAbsSetup::new(AbsoluteAxisType::ABS_HAT0X, abs_hat);
    let abs_hat_y = UinputAbsSetup::new(AbsoluteAxisType::ABS_HAT0Y, abs_hat);

    let mut uhandle = VirtualDeviceBuilder::new()
        .context("Failed to create instance of evdev::VirtualDeviceBuilder")?
        .name(b"Microsoft X-Box 360 pad")
        .input_id(input_id)
        .with_keys(&keys)?
        .with_absolute_axis(&abs_x)?
        .with_absolute_axis(&abs_y)?
        .with_absolute_axis(&abs_rx)?
        .with_absolute_axis(&abs_ry)?
        .with_absolute_axis(&abs_z)?
        .with_absolute_axis(&abs_rz)?
        .with_absolute_axis(&abs_hat_x)?
        .with_absolute_axis(&abs_hat_y)?
        .build()
        .context("Failed to create uinput device")?;

    log_debug("rinputer4_5 starting up");

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || indev_watcher(tx));

    for ev in rx {
        uhandle.emit(&[ev])?;
    }

    Ok(())
}
