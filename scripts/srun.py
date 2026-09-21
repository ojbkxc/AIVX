# -*- coding: utf-8 -*-
"""通用 SSH 执行器：10054 抖动自动重试；跑完打印输出。用法：
   python scripts/srun.py "<remote command>"
"""
import sys, time
sys.stdout.reconfigure(encoding='utf-8', errors='replace')
import paramiko

def ssh():
    last = None
    for _ in range(25):
        try:
            c = paramiko.SSHClient()
            c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            c.connect("104.223.65.202", 10222, "root", "mzyxc8520#", timeout=12)
            return c
        except Exception as e:
            last = e
            time.sleep(3)
    print(f"SSH FAIL: {last}")
    sys.exit(1)

def run(cli, cmd, timeout=60):
    for _ in range(3):
        try:
            _, out, err = cli.exec_command(cmd, timeout=timeout)
            return out.read().decode('utf-8', 'replace'), err.read().decode('utf-8', 'replace')
        except Exception as e:
            print(f"exec retry ({e})", file=sys.stderr)
            time.sleep(2)
    return "", "exec failed"

if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "hostname"
    cli = ssh()
    out, err = run(cli, cmd, timeout=120)
    print(out)
    if err.strip():
        print("STDERR:", err[:1000], file=sys.stderr)
    cli.close()
