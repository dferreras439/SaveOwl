use anyhow::{anyhow, Result};
use std::{collections::HashMap, ffi::OsString, os::windows::ffi::OsStringExt, slice};
use windows::{core::{w, PCWSTR}, Win32::System::Performance::*};

pub struct GpuSampler {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl GpuSampler {
    pub fn new() -> Result<Self> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            let status = PdhOpenQueryW(PCWSTR::null(), 0, &mut query);
            if status != 0 { return Err(anyhow!("PdhOpenQueryW: 0x{status:08x}")); }
            let mut counter = PDH_HCOUNTER::default();
            let status = PdhAddEnglishCounterW(
                query,
                w!(r"\GPU Engine(*)\Utilization Percentage"),
                0,
                &mut counter,
            );
            if status != 0 {
                PdhCloseQuery(query);
                return Err(anyhow!("PdhAddEnglishCounterW: 0x{status:08x}"));
            }
            // Prime rate counters.
            PdhCollectQueryData(query);
            Ok(Self { query, counter })
        }
    }

    pub fn sample(&mut self) -> Result<HashMap<u32, f64>> {
        unsafe {
            let status = PdhCollectQueryData(self.query);
            if status != 0 { return Err(anyhow!("PdhCollectQueryData: 0x{status:08x}")); }

            let mut bytes = 0u32;
            let mut count = 0u32;
            let first = PdhGetFormattedCounterArrayW(
                self.counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None,
            );
            // PDH_MORE_DATA == 0x800007D2. Some systems return success with zero items.
            if first != 0x8000_07D2 && first != 0 { return Err(anyhow!("PdhGetFormattedCounterArrayW(size): 0x{first:08x}")); }
            if bytes == 0 || count == 0 { return Ok(HashMap::new()); }

            // PDH writes an array of structs. Allocate pointer-aligned storage rather than
            // casting a Vec<u8>, whose alignment is only 1.
            let word = std::mem::size_of::<usize>();
            let mut buf = vec![0usize; (bytes as usize + word - 1) / word];
            let status = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                Some(buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>()),
            );
            if status != 0 { return Err(anyhow!("PdhGetFormattedCounterArrayW(data): 0x{status:08x}")); }

            let items = slice::from_raw_parts(buf.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(), count as usize);
            let mut per_pid = HashMap::<u32, f64>::new();
            for item in items {
                let name = pwstr_to_string(item.szName.0);
                let Some(pid) = parse_pid(&name) else { continue; };
                if item.FmtValue.CStatus != 0 { continue; }
                let v = item.FmtValue.Anonymous.doubleValue;
                if v.is_finite() && v > 0.0 {
                    // Multiple engines may be active. Sum and clamp for a simple "is gaming" score.
                    *per_pid.entry(pid).or_default() += v;
                }
            }
            for v in per_pid.values_mut() { *v = v.clamp(0.0, 100.0); }
            Ok(per_pid)
        }
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) { unsafe { PdhCloseQuery(self.query); } }
}

fn parse_pid(instance: &str) -> Option<u32> {
    let s = instance.to_ascii_lowercase();
    let rest = s.split("pid_").nth(1)?;
    rest.split('_').next()?.parse().ok()
}

fn pwstr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() { return String::new(); }
    unsafe {
        let mut len = 0usize;
        while *ptr.add(len) != 0 { len += 1; }
        OsString::from_wide(slice::from_raw_parts(ptr, len)).to_string_lossy().into_owned()
    }
}
