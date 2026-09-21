# -*- coding: utf-8 -*-
"""预览修复线上验证 v2（websockets 库，之前 aiohttp 没装且被旧残留干扰）：
4 路 WS 各订阅 25s 全部出数据；断开 5s 后 ffmpeg 全收（按需启停 + 断开感知）。"""
import sys, time
sys.stdout.reconfigure(encoding='utf-8', errors='replace')
sys.path.insert(0, "scripts")
from srun import ssh, run

SH = r'''
cat > /tmp/verify_preview.py <<'EOF'
import asyncio, time
import websockets

CAMS = ["tp_1-1", "tp_1-2", "tp_2-1", "tp_2-2"]

async def probe(cam):
    uri = f"ws://127.0.0.1:18443/api/stream/{cam}"
    t0 = time.time()
    total = 0
    first = None
    try:
        async with websockets.connect(uri, max_size=10**8) as ws:
            while time.time() - t0 < 25:
                msg = await asyncio.wait_for(ws.recv(), timeout=20)
                total += len(msg)
                if first is None:
                    first = time.time() - t0
    except Exception as e:
        return cam, total, first, "ERR %s" % type(e).__name__
    return cam, total, first, "ok"

async def main():
    results = await asyncio.gather(*[probe(c) for c in CAMS])
    for cam, total, first, st in results:
        fs = "%.1fs" % first if first else "-"
        print("%s: %d bytes, first=%s, %s" % (cam, total, fs, st))

asyncio.run(main())
EOF
python3 /tmp/verify_preview.py
echo "--- 等 6s 让 unsubscribe 落地 ---"
sleep 6
echo "--- 残留 pipe:1 ffmpeg（应为 0）---"
ps -eo pid,args | grep 'pipe:1' | grep -v grep | wc -l
echo "--- healthz ---"
curl -s --max-time 5 http://127.0.0.1:18443/api/healthz | head -c 500
'''

def main():
    cli = ssh()
    out, err = run(cli, SH, timeout=180)
    print(out)
    if err.strip():
        print("stderr:", err[:500])
    cli.close()

main()
