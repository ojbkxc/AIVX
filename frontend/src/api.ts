// AIVX 前端 API 客户端（对齐 AIGX：管理面走 /api/*，不走 /v1/*）。
// 类型安全 + 可注入（测试 mock）。

import type { Device } from './types';

/** API 客户端接口（可注入 mock 测试）。 */
export interface ApiClient {
  listDevices(): Promise<Device[]>;
}

/** 真实实现：fetch /api/devices（后端 18443，Vite 开发代理）。 */
export class RealApiClient implements ApiClient {
  constructor(private baseUrl = '/api') {}

  async listDevices(): Promise<Device[]> {
    const resp = await fetch(`${this.baseUrl}/devices`);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return resp.json();
  }
}

/** 内存 mock（测试用）。 */
export class MockApiClient implements ApiClient {
  constructor(private devices: Device[] = []) {}

  async listDevices(): Promise<Device[]> {
    return this.devices;
  }
}