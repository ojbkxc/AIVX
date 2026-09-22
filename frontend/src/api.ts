// AIVX 前端 API 客户端（对齐 AIGX：管理面走 /api/*，不走 /v1/*）。
// 类型安全 + 可注入（测试 mock）。

import type { AlarmsSummary, Device, Healthz, Recording, SourceConfig } from './types';

/** API 客户端接口（可注入 mock 测试）。 */
export interface ApiClient {
  listDevices(): Promise<Device[]>;
  healthz(): Promise<Healthz>;
  alarmsSummary(): Promise<AlarmsSummary>;
  listRecordings(deviceId: string): Promise<Recording[]>;
  listConfig(): Promise<SourceConfig[]>;
  authStatus(): Promise<AuthStatus>;
  login(password: string): Promise<{ ok: boolean; auth_enabled: boolean }>;
  logout(): Promise<{ ok: boolean }>;
  addDevice(req: AddDeviceReq): Promise<DeviceMutationResult>;
  updateDevice(id: string, req: UpdateDeviceReq): Promise<DeviceMutationResult>;
  deleteDevice(id: string): Promise<DeviceMutationResult>;
}

/** POST /api/devices 请求体（P9-3）。 */
export interface AddDeviceReq {
  id: string;
  rtsp_url: string;
  record_mode?: string;
  retain_days?: number;
}

/** PUT /api/devices/{id} 请求体（P9-4；全字段可选）。 */
export interface UpdateDeviceReq {
  record_mode?: string;
  retain_days?: number;
  classes?: string[];
  enabled?: boolean;
}

/** 设备增删改响应：restart_required=true 时 UI 提示重启生效。 */
export interface DeviceMutationResult {
  ok: boolean;
  restart_required: boolean;
  message: string;
}

/** /api/auth/status 响应（P9-1）。 */
export interface AuthStatus {
  auth_enabled: boolean;
  logged_in: boolean;
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

  async listRecordings(deviceId: string): Promise<Recording[]> {
    return this.get<Recording[]>(`/recordings/${encodeURIComponent(deviceId)}`);
  }

  async listConfig(): Promise<SourceConfig[]> {
    return this.get<SourceConfig[]>('/config');
  }

  async authStatus(): Promise<AuthStatus> {
    return this.get<AuthStatus>('/auth/status');
  }

  async login(password: string): Promise<{ ok: boolean; auth_enabled: boolean }> {
    const resp = await fetch(`${this.baseUrl}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ password }),
    });
    if (!resp.ok) {
      throw new Error(`HTTP ${resp.status}`);
    }
    return resp.json();
  }

  async logout(): Promise<{ ok: boolean }> {
    const resp = await fetch(`${this.baseUrl}/auth/logout`, { method: 'POST' });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return resp.json();
  }

  private async post<T>(path: string, body?: unknown, method = 'POST'): Promise<T> {
    const resp = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (!resp.ok) {
      const msg = await resp.text().catch(() => '');
      throw new Error(msg || `HTTP ${resp.status}`);
    }
    return resp.json();
  }

  async addDevice(req: AddDeviceReq): Promise<DeviceMutationResult> {
    return this.post<DeviceMutationResult>('/devices', req);
  }

  async updateDevice(id: string, req: UpdateDeviceReq): Promise<DeviceMutationResult> {
    return this.post<DeviceMutationResult>(`/devices/${encodeURIComponent(id)}`, req, 'PUT');
  }

  async deleteDevice(id: string): Promise<DeviceMutationResult> {
    return this.post<DeviceMutationResult>(`/devices/${encodeURIComponent(id)}`, undefined, 'DELETE');
  }
}

/** 内存 mock（测试用）。 */
export class MockApiClient implements ApiClient {
  constructor(
    private devices: Device[] = [],
    private healthzData?: Healthz,
    private alarmsData?: AlarmsSummary,
    private recordingsData?: Recording[],
    private configData?: SourceConfig[],
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
    return this.alarmsData ?? { active: 0, projected_events: 3, items: [] };
  }

  async listRecordings(_deviceId: string): Promise<Recording[]> {
    return this.recordingsData ?? [];
  }

  async listConfig(): Promise<SourceConfig[]> {
    return this.configData ?? [];
  }

  async authStatus(): Promise<AuthStatus> {
    return { auth_enabled: false, logged_in: true };
  }

  async login(_password: string): Promise<{ ok: boolean; auth_enabled: boolean }> {
    return { ok: true, auth_enabled: false };
  }

  async logout(): Promise<{ ok: boolean }> {
    return { ok: true };
  }

  async addDevice(_req: AddDeviceReq): Promise<DeviceMutationResult> {
    return { ok: true, restart_required: true, message: 'mock' };
  }

  async updateDevice(_id: string, _req: UpdateDeviceReq): Promise<DeviceMutationResult> {
    return { ok: true, restart_required: false, message: 'mock' };
  }

  async deleteDevice(_id: string): Promise<DeviceMutationResult> {
    return { ok: true, restart_required: true, message: 'mock' };
  }
}
