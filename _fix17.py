p='src-tauri/src/commands/applog.rs'
s=open(p,encoding='utf-8').read()
s=s.replace('use crate::prelude::*;\n\n','')
s=s.replace('use std::sync::atomic::Ordering;','use std::sync::atomic::Ordering;\n\nuse crate::config::AppConfig;')
open(p,'w',encoding='utf-8').write(s)
print(open(p,encoding='utf-8').read()[:300])
