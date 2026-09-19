// I10 能力驱动渲染：设备能力决定 UI——"不支持"是数据不是异常。
// 前端按 capabilities 动态显示 PTZ 云台/事件订阅/音频等控件，
// 而不是后端返回错误。

import type { Capabilities } from '../types';

export function DeviceCapabilities({ caps }: { caps: Capabilities }) {
  return (
    <div className="capabilities" data-testid="device-capabilities">
      <span className={`badge ${caps.ptz ? 'supported' : 'unsupported'}`}>
        PTZ {caps.ptz ? '✓' : '—'}
      </span>
      <span className={`badge ${caps.event_subscription ? 'supported' : 'unsupported'}`}>
        事件订阅 {caps.event_subscription ? '✓' : '—'}
      </span>
      <span className={`badge ${caps.imaging ? 'supported' : 'unsupported'}`}>
        成像 {caps.imaging ? '✓' : '—'}
      </span>
      <span className={`badge ${caps.audio ? 'supported' : 'unsupported'}`}>
        音频 {caps.audio ? '✓' : '—'}
      </span>
      <span className={`badge ${caps.main_sub_streams ? 'supported' : 'unsupported'}`}>
        主/子码流 {caps.main_sub_streams ? '✓' : '—'}
      </span>
    </div>
  );
}