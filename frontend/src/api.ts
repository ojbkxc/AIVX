// AIVX 前端 API 客户端（对齐 AIGX：管理面走 /api/*，不走 /v1/*）。
// 类型安全 + 可注入（测试 mock）。

import type { AlarmsSummary, Device, Healthz, Recording } from './types';

/** API 客户端接口（可注入 mock 测试）。 */
export interface ApiClient {
  listDevices(): Promise<Device[]>;
  healthz(): Promise<Healthz>;
  alarmsSummary(): Promise<AlarmsSummary>;
  listRecordings(deviceId: string): Promise<Recording[]>;
}

/** 真实实现：fetch /api/*（后端 18443，Vite 开发代理）。 */
export class RealApiClient implements ApiClient {
  constructor(private baseUrl = '/api') {}

  private async get<T>(path: string): Promise<T> {
    const resp = await fetch(`${this.baseUrl}${path}`);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return resp.json();
  }

  async listDevices(): Promise<Device[]> {
    return this.get<Device[]>('/devices');
  }

  async healthz(): Promise<Healthz> {
    return this.get<Healthz>('/healthz');
  }

  async alarmsSummary(): Promise<AlarmsSummary> {
    return this.get<AlarmsSummary>('/alarms');
  }

  async listRecordings(_deviceId: string): Promise<Recording[]> {
    // P8d：后端录像索引 API 待接（recordings 投影器读）——当前返回空
    return [];
  }
}

/** 内存 mock（测试用）。 */
export class MockApiClient implements ApiClient {
  constructor(
    private devices: Device[] = [],
    private healthzData?: Healthz,
  ) {}

  async listDevices(): Promise<Device[]> {
    return this.devices;
  }

  async healthz(): Promise<Healthz> {
    return (
      this.healthzData ?? {
        status: 'ok',
        max_seq: 1,
        version: '0.1.0-mock',
        streams: this.devices.map((d, i) => ({
          id: d.id,
          state: 'ok' as const,
          decode_frames: 100 + i,
          inferences: i,
        })),
      }
    );
  }

  async alarmsSummary(): Promise<AlarmsSummary> {
    return { active: 0, projected_events: 3 };
  }

  async listRecordings(_deviceId: string): Promise<Recording[]> {
    return [];
  }
}
