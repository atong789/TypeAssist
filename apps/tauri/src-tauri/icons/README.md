# Icons

**`icon.png` here is currently a 1×1 transparent placeholder** so `tauri::generate_context!()` will compile. Replace it before shipping.

Tauri's bundler also wants ICNS / ICO / PSD variants for distribution. To generate the full set from a 1024×1024 master:

```bash
npm run tauri icon path/to/master-1024.png
```

That command overwrites `icon.png` and emits the platform-specific icon files alongside it.
