# -*- coding: utf-8 -*-
"""通用部署：$TEMP/aivx-bundle8 的 tar.gz 上传服务器并替换重启 + 验证。"""
import sys, time, os
sys.stdout.reconfigure(encoding='utf-8', errors='replace')
sys.path.insert(0, "scripts")
from srun import ssh, run

TGZ = os.path.join(os.environ.get("TEMP", "/tmp"), "aivx-bundle8", "aivx-linux-amd64.tar.gz")

def main():
    cli = ssh()
    sftp = cli.open_sftp()
    sftp.put(TGZ, "/tmp/aivx-deploy.tar.gz")
    print("uploaded", os.path.getsize(TGZ))
    steps = [
        ("解包", "rm -rf /tmp/aivx-new && mkdir -p /tmp/aivx-new && tar -xzf /tmp/aivx-deploy.tar.gz -C /tmp/aivx-new"),
        ("停服", "systemctl stop aivx"),
        ("替换", "cp /tmp/aivx-new/aivx/aivx /opt/aivx/aivx && chmod +x /opt/aivx/aivx && cp -r /tmp/aivx-new/aivx/static/. /opt/aivx/static/"),
        ("重启", "systemctl start aivx && sleep 2 && systemctl is-active aivx"),
        ("日志", "journalctl -u aivx --since '-20 sec' --no-pager | tail -5"),
    ]
    for name, cmd in steps:
        out, err = run(cli, cmd, timeout=90)
        print(f"== {name} ==")
        print(out.strip()[-800:] if out.strip() else "(ok)")
        if err.strip():
            print("err:", err.strip()[-300:])
    cli.close()

main()
