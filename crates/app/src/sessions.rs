//! Finds the apps that are currently playing audio, from Windows' per-device audio sessions, and
//! asks Windows to say the moment an app opens a session or starts or stops playing, so a new app's
//! first words aren't lost waiting for the next poll, and when audio devices come, go or change default.

use std::{collections::HashSet, sync::mpsc::Sender};

use windows::{
    core::{implement, Interface, Ref, Result as ComResult, GUID, PCWSTR},
    Win32::{
        Media::Audio::{
            eRender, AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateActive, EDataFlow, ERole,
            IAudioSessionControl, IAudioSessionControl2, IAudioSessionEvents, IAudioSessionEvents_Impl,
            IAudioSessionManager2, IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDeviceEnumerator,
            IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
        },
        Foundation::PROPERTYKEY,
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL},
    },
};

use kiku_core::engine::Result;

use crate::processes::{root_of, Process};

/// An app playing audio, identified by its main process.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub pid: u32,
    pub name: String,
}

/// The audio sessions of every output device, with Windows asked to send `wake` whenever a session
/// opens, starts or stops playing, or a device is added, removed or made the default. Unsubscribes
/// when dropped. Needs COM initialized on this thread.
pub struct Sessions {
    wake: Sender<()>,
    endpoints: Option<WatchedEndpoints>,
    devices: Vec<WatchedDevice>,
    sessions: Vec<WatchedSession>,
}

struct WatchedEndpoints {
    enumerator: IMMDeviceEnumerator,
    news: IMMNotificationClient,
}

struct WatchedDevice {
    id: String,
    manager: IAudioSessionManager2,
    news: IAudioSessionNotification,
}

struct WatchedSession {
    id: String,
    control: IAudioSessionControl,
    events: IAudioSessionEvents,
}

impl Sessions {
    pub fn new(wake: Sender<()>) -> Self {
        Self { wake, endpoints: None, devices: Vec::new(), sessions: Vec::new() }
    }

    /// Apps with an active audio session on any output device, one entry per main process. Skips system
    /// sounds and this program itself. Also starts watching devices and sessions not watched yet.
    pub fn playing_apps(&mut self, processes: &[Process]) -> Result<Vec<App>> {
        let mut apps: Vec<App> = Vec::new();
        for pid in self.active_session_pids()? {
            let Some(root) = root_of(pid, processes) else { continue };
            if root.pid != std::process::id() && !apps.iter().any(|app| app.pid == root.pid) {
                apps.push(App { pid: root.pid, name: display_name(&root.exe) });
            }
        }
        Ok(apps)
    }

    fn active_session_pids(&mut self) -> Result<Vec<u32>> {
        let mut pids = Vec::new();
        let (mut seen, mut seen_devices) = (HashSet::new(), HashSet::new());
        let devices: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        self.watch_endpoints(&devices)?;
        let collection = unsafe { devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
        for index in 0..unsafe { collection.GetCount()? } {
            let device = unsafe { collection.Item(index)? };
            let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None)? };
            // Windows only sends session news once the sessions have been enumerated, so list first.
            let sessions = unsafe { manager.GetSessionEnumerator()? };
            let device_id = owned_string(unsafe { device.GetId()? });
            seen_devices.insert(device_id.clone());
            self.watch_device(device_id, &manager)?;
            for index in 0..unsafe { sessions.GetCount()? } {
                let control = unsafe { sessions.GetSession(index)? };
                let details: IAudioSessionControl2 = control.cast()?;
                let id = owned_string(unsafe { details.GetSessionInstanceIdentifier()? });
                seen.insert(id.clone());
                self.watch_session(id, &control)?;
                // PID 0 is the system-sounds session.
                let pid = unsafe { details.GetProcessId() }.unwrap_or(0);
                if pid != 0 && unsafe { control.GetState()? } == AudioSessionStateActive {
                    pids.push(pid);
                }
            }
        }
        // Sessions that closed and devices that went away need no more watching.
        self.sessions.retain(|watched| seen.contains(&watched.id));
        self.devices.retain(|watched| seen_devices.contains(&watched.id));
        Ok(pids)
    }

    fn watch_endpoints(&mut self, devices: &IMMDeviceEnumerator) -> Result<()> {
        if self.endpoints.is_none() {
            let news: IMMNotificationClient = Wake(self.wake.clone()).into();
            unsafe { devices.RegisterEndpointNotificationCallback(&news)? };
            self.endpoints = Some(WatchedEndpoints { enumerator: devices.clone(), news });
        }
        Ok(())
    }

    fn watch_device(&mut self, id: String, manager: &IAudioSessionManager2) -> Result<()> {
        if self.devices.iter().any(|d| d.id == id) {
            return Ok(());
        }
        let news: IAudioSessionNotification = Wake(self.wake.clone()).into();
        unsafe { manager.RegisterSessionNotification(&news)? };
        self.devices.push(WatchedDevice { id, manager: manager.clone(), news });
        Ok(())
    }

    fn watch_session(&mut self, id: String, control: &IAudioSessionControl) -> Result<()> {
        if self.sessions.iter().any(|s| s.id == id) {
            return Ok(());
        }
        let events: IAudioSessionEvents = Wake(self.wake.clone()).into();
        unsafe { control.RegisterAudioSessionNotification(&events)? };
        self.sessions.push(WatchedSession { id, control: control.clone(), events });
        Ok(())
    }
}

impl Drop for WatchedEndpoints {
    fn drop(&mut self) {
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.news) };
    }
}

impl Drop for WatchedDevice {
    fn drop(&mut self) {
        let _ = unsafe { self.manager.UnregisterSessionNotification(&self.news) };
    }
}

impl Drop for WatchedSession {
    fn drop(&mut self) {
        let _ = unsafe { self.control.UnregisterAudioSessionNotification(&self.events) };
    }
}

/// Sent by Windows when a session opens on a device, a watched session starts or stops playing, or a
/// device is added, removed or made the default (the mic may have to move to another device).
/// All just wake discovery, which looks again; Windows advises against doing more in these callbacks.
#[implement(IAudioSessionNotification, IAudioSessionEvents, IMMNotificationClient)]
struct Wake(Sender<()>);

impl Wake {
    fn wake(&self) -> ComResult<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

impl IAudioSessionNotification_Impl for Wake_Impl {
    fn OnSessionCreated(&self, _session: Ref<IAudioSessionControl>) -> ComResult<()> {
        self.wake()
    }
}

impl IAudioSessionEvents_Impl for Wake_Impl {
    fn OnStateChanged(&self, _state: AudioSessionState) -> ComResult<()> {
        self.wake()
    }

    fn OnDisplayNameChanged(&self, _name: &PCWSTR, _context: *const GUID) -> ComResult<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _path: &PCWSTR, _context: *const GUID) -> ComResult<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(&self, _volume: f32, _mute: windows::core::BOOL, _context: *const GUID) -> ComResult<()> {
        Ok(())
    }

    fn OnChannelVolumeChanged(&self, _count: u32, _volumes: *const f32, _channel: u32, _context: *const GUID) -> ComResult<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _grouping: *const GUID, _context: *const GUID) -> ComResult<()> {
        Ok(())
    }

    fn OnSessionDisconnected(&self, _reason: AudioSessionDisconnectReason) -> ComResult<()> {
        self.wake()
    }
}

impl IMMNotificationClient_Impl for Wake_Impl {
    fn OnDeviceStateChanged(&self, _device: &PCWSTR, _state: DEVICE_STATE) -> ComResult<()> {
        self.wake()
    }

    fn OnDeviceAdded(&self, _device: &PCWSTR) -> ComResult<()> {
        self.wake()
    }

    fn OnDeviceRemoved(&self, _device: &PCWSTR) -> ComResult<()> {
        self.wake()
    }

    fn OnDefaultDeviceChanged(&self, _flow: EDataFlow, _role: ERole, _device: &PCWSTR) -> ComResult<()> {
        self.wake()
    }

    fn OnPropertyValueChanged(&self, _device: &PCWSTR, _key: &PROPERTYKEY) -> ComResult<()> {
        Ok(())
    }
}

/// Copies a string Windows allocated, and frees it.
fn owned_string(text: windows::core::PWSTR) -> String {
    let owned = unsafe { text.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(text.0 as _)) };
    owned
}

/// "opera.exe" -> "Opera": an app's name as its captions are labelled.
pub fn display_name(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe);
    let mut chars = stem.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
