import re
p='src-tauri/src/commands/diag.rs'
s=open(p,encoding='utf-8').read()
s=s.replace('let h = screen_capture_plugin::handle()','let h = crate::screen_capture_plugin::handle()')
open(p,'w',encoding='utf-8').write(s)
p='src-tauri/src/commands/notify.rs'
s=open(p,encoding='utf-8').read()
s=s.replace('let h = screen_capture_plugin::handle()','let h = crate::screen_capture_plugin::handle()')
open(p,'w',encoding='utf-8').write(s)
p='src-tauri/src/lib.rs'
s=open(p,encoding='utf-8').read()
s=s.replace('    pub(crate) use crate::config::{AppConfig, Channel};','    pub(crate) use crate::config::Channel;')
open(p,'w',encoding='utf-8').write(s)
print('patched android-only paths')
