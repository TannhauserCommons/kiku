//! App icons, taken from each app's executable, and Kiku's own, cached as Direct2D bitmaps.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use windows::{
    core::{Result, HSTRING},
    Win32::{
        Graphics::{
            Direct2D::{ID2D1Bitmap, ID2D1RenderTarget},
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
                WICBitmapPaletteTypeMedianCut,
            },
        },
        Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        UI::{
            Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
            WindowsAndMessaging::{DestroyIcon, HICON},
        },
    },
};

use super::app_icon::app_icon;

pub struct Icons {
    wic: IWICImagingFactory,
    /// `None` records an app whose icon could not be loaded, so it isn't retried every frame.
    cache: HashMap<PathBuf, Option<ID2D1Bitmap>>,
    /// Kiku's icon, by size in pixels.
    own: HashMap<i32, Option<ID2D1Bitmap>>,
}

impl Icons {
    /// Needs COM initialized on this thread.
    pub fn new() -> Result<Self> {
        let wic = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self { wic, cache: HashMap::new(), own: HashMap::new() })
    }

    /// The icon of the executable at `exe`, as a bitmap usable on `target`.
    pub fn get(&mut self, target: &ID2D1RenderTarget, exe: &Path) -> Option<ID2D1Bitmap> {
        if !self.cache.contains_key(exe) {
            let bitmap = self.load(target, exe).ok();
            self.cache.insert(exe.to_owned(), bitmap);
        }
        self.cache[exe].clone()
    }

    /// Kiku's icon at `size` pixels, as a bitmap usable on `target`.
    pub fn kiku(&mut self, target: &ID2D1RenderTarget, size: i32) -> Option<ID2D1Bitmap> {
        if !self.own.contains_key(&size) {
            let bitmap = app_icon(size).and_then(|icon| self.bitmap_from(target, icon)).ok();
            self.own.insert(size, bitmap);
        }
        self.own[&size].clone()
    }

    fn load(&self, target: &ID2D1RenderTarget, exe: &Path) -> Result<ID2D1Bitmap> {
        let mut info = SHFILEINFOW::default();
        let found = unsafe {
            SHGetFileInfoW(
                &HSTRING::from(exe.as_os_str()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if found == 0 || info.hIcon.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }
        self.bitmap_from(target, info.hIcon)
    }

    /// Converts `icon` to a bitmap usable on `target`, and frees it.
    fn bitmap_from(&self, target: &ID2D1RenderTarget, icon: HICON) -> Result<ID2D1Bitmap> {
        let bitmap = unsafe { self.wic.CreateBitmapFromHICON(icon) };
        unsafe { DestroyIcon(icon).ok() };
        // Direct2D draws premultiplied alpha; icons come straight-alpha.
        let converter = unsafe { self.wic.CreateFormatConverter()? };
        unsafe {
            converter.Initialize(&bitmap?, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeMedianCut)?;
            target.CreateBitmapFromWicBitmap(&converter, None)
        }
    }
}
