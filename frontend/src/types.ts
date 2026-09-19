// AIVX 前端类型（对齐后端 aivx-net 的领域模型）。

/** 设备能力（I10：前端按此动态渲染 UI——"不支持"是数据不是异常）。 */
export interface Capabilities {
  ptz: boolean;
  event_subscription: boolean;
  imaging: boolean;
  audio: boolean;
  main_sub_streams: boolean;
}

/** 设备条目（对齐后端 Device）。 */
export interface Device {
  id: string;
  name: string;
  access_type: 'onvif' | 'rtsp' | 'gb28181';
  onvif_url: string | null;
  rtsp_main: string | null;
  rtsp_sub: string | null;
  manufacturer: string | null;
  model: string | null;
  capabilities: Capabilities;
  /** P8d：录像模式（后端 CameraHandle 派生） */
  record_mode?: 'off' | 'always' | 'motion';
}

/** 报警条目。 */
export interface Alarm {
  id: string;
  device_id: string;
  event_type: string;
  description: string;
  ts: number;
}

/** 录像片段。 */
export interface Recording {
  id: string;
  device_id: string;
  file_path: string;
  start_ts: number;
  duration_secs: number;
}

/** healthz 的单路流状态（P8d：预览页数据源）。 */
export interface StreamStatus {
  id: string;
  state: 'ok' | 'connecting' | 'reconnecting' | 'degraded' | 'stopped';
  decode_frames: number;
  inferences: number;
}

/** healthz 响应。 */
export interface Healthz {
  status: string;
  max_seq: number;
  version: string;
  streams?: StreamStatus[];
}

/** 报警汇总（/api/alarms）。 */
export interface AlarmsSummary {
  active: number;
  projected_events: number;
}