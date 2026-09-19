//! 设备驱动注册表（DESIGN.md §12 / P6）——对齐 open-nvr `camera_drivers/registry.py`。
//!
//! 选择顺序（抄 open-nvr registry.py）：
//!   1. 厂商名匹配：`manufacturer.matches()`（如 "TP-LINK"/"Hikvision"）→ 确认
//!   2. 能力指纹：对 OEM 贴牌（厂商名说谎）逐驱动 `probe()` 按优先级试
//!   3. ONVIF 兜底：任何未知厂商都回落 ONVIF 通用驱动
//!
//! 返回的是 `Box<dyn DeviceAdapter>`——驱动按需实例化，不引重依赖。
//! I10 保持：驱动只报能力（Supported::No 是数据），无 set_ip/factory_reset。

use crate::{AdapterError, Capabilities, Device, DeviceAdapter, DeviceCandidate};

/// 驱动元数据（注册表条目）。
#[derive(Clone)]
pub struct DriverSpec {
    pub name: &'static str,
    /// 厂商名匹配：返回 true 表示该驱动认识这个厂商。
    pub matches_manufacturer: fn(&str) -> bool,
    /// 能力探测（对未知厂商做指纹确认）。返回 Some(caps) 表示确认是该厂商。
    pub probe: Option<fn(&Device) -> Option<Capabilities>>,
    /// 优先级（值越大越优先，用于 OEM 指纹确认的遍历顺序）。
    pub priority: u8,
    /// 构造驱动（P6 返回桩驱动；P7 接真实 ONVIF/SDK 实现）。
    pub factory: fn() -> Box<dyn DeviceAdapter>,
}

/// 驱动注册表。
pub struct Registry {
    specs: Vec<DriverSpec>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Registry {
    /// 内置驱动表。当前只有 ONVIF 兜底；厂商驱动（海康/大华）随 SDK feature 追加。
    pub fn builtin() -> Self {
        // ONVIF 是兜底，优先级最低。
        let onvif = DriverSpec {
            name: "onvif",
            matches_manufacturer: |_| false,
            probe: None,
            priority: 0,
            factory: || Box::new(crate::OnvifAdapter),
        };
        // 占位：海康/大华驱动在 SDK feature 下追加（P6 之后的扩展点）。
        Self { specs: vec![onvif] }
    }

    /// 为设备选驱动：厂商匹配 → 指纹 probe → ONVIF 兜底。
    ///
    /// 返回 `(驱动名, Box<dyn DeviceAdapter>)`。
    pub fn select(&self, device: &Device) -> (&'static str, Box<dyn DeviceAdapter>) {
        // 1. 厂商名精确匹配
        if let Some(manufacturer) = device.manufacturer.as_deref() {
            for spec in self.specs.iter() {
                if (spec.matches_manufacturer)(manufacturer) {
                    return (spec.name, (spec.factory)());
                }
            }
        }
        // 2. 指纹 probe（OEM 贴牌：厂商名说谎但能力特征可辨）——按优先级降序
        let mut by_priority = self.specs.clone();
        by_priority.sort_by(|a, b| b.priority.cmp(&a.priority));
        for spec in by_priority.iter() {
            if let Some(probe) = spec.probe {
                if probe(device).is_some() {
                    return (spec.name, (spec.factory)());
                }
            }
        }
        // 3. ONVIF 兜底（总是存在）
        let fallback = self
            .specs
            .iter()
            .find(|s| s.name == "onvif")
            .expect("onvif 兜底驱动必须存在");
        (fallback.name, (fallback.factory)())
    }

    /// 可用驱动列表（前端下拉/诊断用）。
    pub fn list(&self) -> Vec<&'static str> {
        self.specs.iter().map(|s| s.name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_with_manufacturer(m: &str) -> Device {
        Device {
            id: "d1".into(),
            name: "cam".into(),
            access_type: crate::AccessType::Onvif,
            onvif_url: Some("http://1.2.3.4/onvif".into()),
            rtsp_main: None,
            rtsp_sub: None,
            manufacturer: Some(m.into()),
            model: None,
            capabilities: Capabilities::default(),
        }
    }

    /// P6 契约：未知厂商回落 ONVIF 兜底。
    #[test]
    fn unknown_manufacturer_falls_back_to_onvif() {
        let reg = Registry::builtin();
        let dev = dev_with_manufacturer("SomeVendor");
        let (name, adapter) = reg.select(&dev);
        assert_eq!(name, "onvif");
        // 兜底驱动能正常探测能力（空能力 = 数据不是错误）
        let caps = adapter.capabilities(&dev).unwrap();
        assert!(!caps.ptz);
    }

    /// P6 契约：厂商名精确匹配优先于兜底。
    #[test]
    fn manufacturer_match_wins() {
        // 构造一个认识 "TP-LINK" 的注册表（模拟未来加厂商驱动）
        let reg = Registry {
            specs: vec![
                DriverSpec {
                    name: "tplink",
                    matches_manufacturer: |m| m == "TP-LINK",
                    probe: None,
                    priority: 1,
                    factory: || Box::new(crate::OnvifAdapter),
                },
                DriverSpec {
                    name: "onvif",
                    matches_manufacturer: |_| false,
                    probe: None,
                    priority: 0,
                    factory: || Box::new(crate::OnvifAdapter),
                },
            ],
        };
        let dev = dev_with_manufacturer("TP-LINK");
        let (name, _) = reg.select(&dev);
        assert_eq!(name, "tplink");
    }

    /// P6 契约：OEM 贴牌（厂商名不认识但 probe 特征匹配）走指纹确认。
    #[test]
    fn oem_rebadge_matches_by_probe() {
        // 某厂商驱动：probe 发现设备有某特征（如特定 ONVIF 服务）即认领
        let reg = Registry {
            specs: vec![
                DriverSpec {
                    name: "hik-variant",
                    matches_manufacturer: |m| m == "Hikvision",
                    probe: Some(|d: &Device| {
                        if d.onvif_url
                            .as_deref()
                            .map(|u| u.contains(":8000"))
                            .unwrap_or(false)
                        {
                            Some(Capabilities {
                                ptz: true,
                                ..Default::default()
                            })
                        } else {
                            None
                        }
                    }),
                    priority: 2,
                    factory: || Box::new(crate::OnvifAdapter),
                },
                DriverSpec {
                    name: "onvif",
                    matches_manufacturer: |_| false,
                    probe: None,
                    priority: 0,
                    factory: || Box::new(crate::OnvifAdapter),
                },
            ],
        };
        // 厂商名说谎（贴牌）但 ONVIF 端口特征匹配 → 指纹认领
        let mut dev = dev_with_manufacturer("OEM-Unknown");
        dev.onvif_url = Some("http://1.2.3.4:8000/onvif".into());
        let (name, _) = reg.select(&dev);
        assert_eq!(name, "hik-variant");
    }
}
