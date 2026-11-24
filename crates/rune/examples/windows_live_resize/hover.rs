//! Measures the visible control and tooltip pixels on the same clock as Rune's frame trace.

use std::{
    ptr,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, GetDC, GetPixel, HDC, ReleaseDC};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, SetCursorPos,
};

use super::windows::{checked, client_size, focus_window, rect, set_left_button};

#[derive(Clone, Copy)]
enum Edge {
    Bottom,
    Right,
}

impl Edge {
    fn point(self, outer: RECT, offset: i32) -> POINT {
        match self {
            Self::Bottom => POINT {
                x: (outer.left + outer.right) / 2,
                y: outer.bottom - 5 + offset,
            },
            Self::Right => POINT {
                x: outer.right - 5 + offset,
                y: (outer.top + outer.bottom) / 2,
            },
        }
    }
}

fn qpc_ns() -> u64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut value = 0;
        // SAFETY: writes to the local counter frequency.
        assert_ne!(unsafe { QueryPerformanceFrequency(&mut value) }, 0);
        value
    });
    let mut now = 0;
    // SAFETY: writes to the local counter reading.
    assert_ne!(unsafe { QueryPerformanceCounter(&mut now) }, 0);
    (now as i128 * 1_000_000_000 / frequency as i128) as u64
}

fn cursor(point: POINT) -> Result<(), String> {
    // SAFETY: coordinates refer to the foreground fixture's window or sizing edge.
    checked(
        unsafe { SetCursorPos(point.x, point.y) },
        "SetCursorPos(sweep)",
    )
}

fn require_cursor(hwnd: HWND, expected: POINT) -> Result<(), String> {
    let mut actual = POINT::default();
    // SAFETY: writes to the local point, and queries the foreground HWND without taking ownership.
    checked(unsafe { GetCursorPos(&mut actual) }, "GetCursorPos(sweep)")?;
    if unsafe { GetForegroundWindow() } != hwnd || (actual.x, actual.y) != (expected.x, expected.y)
    {
        return Err(
            "the sweep lost the foreground window or the pointer moved independently".into(),
        );
    }
    Ok(())
}

fn from_client(hwnd: HWND, mut point: POINT) -> Result<POINT, String> {
    // SAFETY: the fixture owns the writable point and only targets its verified window handle.
    checked(
        unsafe { ClientToScreen(hwnd, &mut point) },
        "ClientToScreen(sweep)",
    )?;
    Ok(point)
}

struct DesktopPixels(HDC);

impl DesktopPixels {
    fn new() -> Result<Self, String> {
        // SAFETY: acquire a desktop DC for reading the target's visible test pixels.
        let dc = unsafe { GetDC(ptr::null_mut()) };
        if dc.is_null() {
            Err("could not read desktop pixels".into())
        } else {
            Ok(Self(dc))
        }
    }

    fn read(&self, point: POINT) -> Result<u32, String> {
        // SAFETY: this DC is live until the probe returns.
        let color = unsafe { GetPixel(self.0, point.x, point.y) };
        if color == u32::MAX {
            Err("could not sample a desktop pixel".into())
        } else {
            Ok(color)
        }
    }
}

impl Drop for DesktopPixels {
    fn drop(&mut self) {
        // SAFETY: release the DC exactly once on the acquiring thread.
        unsafe {
            ReleaseDC(ptr::null_mut(), self.0);
        }
    }
}

#[derive(Clone, Copy)]
struct Target {
    button: POINT,
    tip: POINT,
}

fn parse(spec: &str) -> Result<(Duration, [Target; 3]), String> {
    let (dwell, targets) = spec
        .split_once('@')
        .ok_or("use --sweep=ms@x,y,tip_x,tip_y;…")?;
    let dwell = dwell.parse::<u64>().map_err(|_| "invalid dwell time")?;
    if !(120..=1500).contains(&dwell) {
        return Err("dwell must be 120 to 1500 ms".into());
    }
    let targets = targets
        .split(';')
        .map(|item| {
            let coordinates = item
                .split(',')
                .map(str::parse::<i32>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "invalid coordinate")?;
            let [x, y, tx, ty] = coordinates.as_slice() else {
                return Err("use four coordinates per target");
            };
            Ok(Target {
                button: POINT { x: *x, y: *y },
                tip: POINT { x: *tx, y: *ty },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let targets: [Target; 3] = targets.try_into().map_err(|_| "use three targets")?;
    Ok((Duration::from_millis(dwell), targets))
}

fn drag(hwnd: HWND, edge: Edge, delta: i32) -> Result<(u64, (i32, i32)), String> {
    let outer = rect(hwnd, false)?;
    let initial = client_size(hwnd)?;
    let start = edge.point(outer, 0);
    cursor(start)?;
    require_cursor(hwnd, start)?;
    if let Err(error) = set_left_button(true) {
        let _ = set_left_button(false);
        return Err(error);
    }
    let moved = (|| {
        thread::sleep(Duration::from_millis(80));
        for step in 1..=12 {
            cursor(edge.point(outer, delta * step / 12))?;
            thread::sleep(Duration::from_millis(18));
        }
        Ok::<(), String>(())
    })();
    let release = qpc_ns();
    let released = set_left_button(false);
    moved?;
    released?;
    Ok((release, initial))
}

fn sweep(
    hwnd: HWND,
    pixels: Option<&DesktopPixels>,
    targets: [Target; 3],
    name: &str,
    release: u64,
    dwell: Duration,
) -> Result<(), String> {
    for (i, target) in targets.into_iter().enumerate() {
        // Move through the first two targets at the supplied cadence, then hold the third
        // long enough to observe a late update on a quick pass.
        let dwell = if i == 2 {
            dwell.max(Duration::from_millis(1000))
        } else {
            dwell
        };
        let button = from_client(hwnd, target.button)?;
        let tip = from_client(hwnd, target.tip)?;
        let (before_button, before_tip) = if let Some(pixels) = pixels {
            (pixels.read(button)?, pixels.read(tip)?)
        } else {
            // Keep the same button-to-button cadence during independent video sampling.
            thread::sleep(Duration::from_millis(30));
            (0, 0)
        };
        let window_started = Instant::now();
        let entered = qpc_ns();
        cursor(button)?;
        let mut button_change = None;
        let mut tip_change = None;
        while window_started.elapsed() < dwell {
            require_cursor(hwnd, button)?;
            if let Some(pixels) = pixels.filter(|_| button_change.is_none()) {
                let color = pixels.read(button)?;
                if color != before_button {
                    button_change = Some((qpc_ns(), color));
                }
            }
            if let Some(pixels) = pixels.filter(|_| tip_change.is_none()) {
                let color = pixels.read(tip)?;
                if color != before_tip {
                    tip_change = Some((qpc_ns(), color));
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        require_cursor(hwnd, button)?;
        let (button_at, button_color) = button_change.unwrap_or((0, before_button));
        let (tip_at, tip_color) = tip_change.unwrap_or((0, before_tip));
        eprintln!(
            "HOVER_SWEEP phase={name} target={} at={entered} release={release} fill={button_at} tip={tip_at} fill_rgb={before_button:06x}/{button_color:06x} tip_rgb={before_tip:06x}/{tip_color:06x}",
            i + 1
        );
    }
    Ok(())
}

pub(super) fn run(hwnd: HWND, spec: &str) -> Result<(), String> {
    let (dwell, targets) = parse(spec)?;
    let client = rect(hwnd, true)?;
    for point in targets
        .into_iter()
        .flat_map(|target| [target.button, target.tip])
    {
        if point.x < 0 || point.y < 0 || point.x >= client.right || point.y >= client.bottom {
            return Err("sample point lies outside the target client".into());
        }
    }
    focus_window(hwnd)?;
    cursor(from_client(hwnd, POINT { x: 300, y: 140 })?)?;
    thread::sleep(Duration::from_millis(200));
    let pixels = (!rune::env::flag("RUNE_HOVER_NO_GDI"))
        .then(DesktopPixels::new)
        .transpose()?;
    sweep(hwnd, pixels.as_ref(), targets, "baseline", 0, dwell)?;
    for (name, edge, delta) in [
        ("vertical-shrink", Edge::Bottom, -128),
        ("horizontal-shrink", Edge::Right, -128),
        ("vertical-grow", Edge::Bottom, 128),
        ("horizontal-grow", Edge::Right, 128),
    ] {
        let (released, before) = drag(hwnd, edge, delta)?;
        sweep(hwnd, pixels.as_ref(), targets, name, released, dwell)?;
        let after = client_size(hwnd)?;
        eprintln!("HOVER_RESIZE phase={name} before={before:?} after={after:?}");
        let expected = match edge {
            Edge::Bottom => (before.0, before.1 + delta),
            Edge::Right => (before.0 + delta, before.1),
        };
        if after != expected {
            return Err(format!("{name} got {after:?}, expected {expected:?}"));
        }
    }
    Ok(())
}
