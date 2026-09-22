# -*- coding: utf-8 -*-
"""下载最新 Deploy artifact（302 剥 Authorization 法）并部署到家里服务器。"""
import sys, time, json, urllib.request, subprocess, os
sys.stdout.reconfigure(encoding='utf-8', errors='replace')

# token 从环境变量取（勿硬编码——push protection 会拦）
TOKEN = os.environ.get("GITHUB_TOKEN")
if not TOKEN:
    print("需要 GITHUB_TOKEN 环境变量"); sys.exit(1)
HEADERS = {"Authorization": f"token {TOKEN}", "Accept": "application/vnd.github+json"}
TEMP = os.environ.get("TEMP", "/tmp")

def api(url):
    req = urllib.request.Request(url, headers=HEADERS)
    return json.load(urllib.request.urlopen(req, timeout=30))

# 最新 Deploy run 的 linux zip artifact（动态查最新 Deploy run——曾硬编码
# 旧 run id 导致部署旧二进制，线上 404 诡异排查浪费一轮）
runs = api("https://api.github.com/repos/ojbkxc/AIVX/actions/runs?per_page=20")
deploy = next(
    r for r in runs["workflow_runs"]
    if r["name"] == "Deploy" and r["conclusion"] == "success"
)
print("deploy run:", deploy["id"], deploy["head_sha"][:7])
arts = api(f"https://api.github.com/repos/ojbkxc/AIVX/actions/runs/{deploy['id']}/artifacts")
items = [a for a in arts.get("artifacts", []) if "linux" in a["name"] or "aivx" in a["name"].lower()]
if not items:
    print("artifacts:", [a["name"] for a in arts.get("artifacts", [])]); sys.exit(1)
art = items[0]
print("artifact:", art["name"], art["id"])

# 302 剥离：NoRedir opener 拿 Location
class NoRedir(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k): return None
opener = urllib.request.build_opener(NoRedir)
req = urllib.request.Request(f"https://api.github.com/repos/ojbkxc/AIVX/actions/artifacts/{art['id']}/zip",
                             headers=HEADERS)
try:
    opener.open(req, timeout=30)
    print("no redirect?"); sys.exit(1)
except urllib.error.HTTPError as e:
    if e.code != 302:
        print("unexpected:", e.code); sys.exit(1)
    loc = e.headers["Location"]

zip_path = os.path.join(TEMP, "aivx-preview-fix.zip")
subprocess.run(["curl", "-sL", "-o", zip_path, loc], check=True)
print("zip:", zip_path, os.path.getsize(zip_path), "bytes")

# 解包
dst = os.path.join(TEMP, "aivx-bundle7")
subprocess.run(["rm", "-rf", dst], check=True)
os.makedirs(dst, exist_ok=True)
subprocess.run(["unzip", "-o", zip_path, "-d", dst], check=True)
for root, dirs, files in os.walk(dst):
    for f in files:
        print(" ", os.path.join(root, f).replace(dst, ""))
