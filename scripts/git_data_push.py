# -*- coding: utf-8 -*-
"""git data API push：github.com 主站 SSL 挂而 api.github.com 正常时的推送通道。
GET /commits/main 取 parent+tree → POST /git/blobs → POST /git/trees（base_tree 增量）
→ POST /git/commits → PATCH /git/refs/heads/main。
用法：python scripts/git_data_push.py <本地 commit SHA>...
"""
import sys, json, urllib.request, subprocess, os
sys.stdout.reconfigure(encoding='utf-8', errors='replace')

TOKEN = os.environ.get("GITHUB_TOKEN", "")
REPO = "ojbkxc/AIVX"
API = "https://api.github.com"
HDR = {"Authorization": f"token {TOKEN}", "Accept": "application/vnd.github+json",
       "Content-Type": "application/json"}

def api(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{API}/repos/{REPO}/{path}", data=data, headers=HDR, method=method)
    try:
        return json.load(urllib.request.urlopen(req, timeout=60))
    except urllib.error.HTTPError as e:
        print("HTTP", e.code, e.read().decode()[:500]); sys.exit(1)

def ls_tree_files(sha):
    """递归列出 commit 的全部 (path, blob_sha)。用 git 命令本地取（可靠）。"""
    out = git_out(["ls-tree", "-r", sha])
    entries = []
    for line in out.splitlines():
        meta, path = line.split("\t", 1)
        mode, typ, bsha = meta.split()
        if typ == "blob":
            entries.append((path, bsha))
    return entries

def git_out(args):
    """git 输出恒 utf-8 解码（Windows 默认 GBK 会炸中文 commit message）。"""
    r = subprocess.run(["git"] + args, capture_output=True, check=True)
    return r.stdout.decode("utf-8", "replace")

def main():
    shas = sys.argv[1:]
    if not shas:
        print("usage: git_data_push.py <commit>..."); sys.exit(1)

    for sha in shas:
        out = git_out(["show", "-s", "--format=%H%n%T%n%P%n%B", sha])
        info = out.split("\n", 3)
        commit_sha, tree_sha, parent, msg = info[0], info[1], info[2], info[3].strip()
        print(f"pushing {commit_sha[:8]}: {msg.splitlines()[0]}")

        # 服务器当前 main（首个 commit 的 parent 必须是远端 HEAD）
        remote = api("GET", "commits/main")
        cur_head = remote["sha"]
        # commits endpoint 不带 tree 字段——从 commit 对象里取
        cur_tree = remote["commit"]["tree"]["sha"]
        if parent != cur_head:
            print(f"parent {parent[:8]} != 远端 HEAD {cur_head[:8]}（先推前置提交）"); sys.exit(1)

        # 增量 tree：本地 commit 相对 parent 的变更文件。
        # blob 本地存在≠服务器有（本地 blob 库与远端不同步）——用内容直传
        # POST /git/blobs 让服务器自己算 sha，避免 422 tree.sha not valid。
        diff = git_out(["diff", "--name-status", parent, commit_sha])
        tree_items = []
        for line in diff.splitlines():
            if not line.strip(): continue
            parts = line.split("\t")
            if parts[0].startswith("R"):  # R100 old new
                old, new = parts[1], parts[2]
                tree_items.append({"path": old, "sha": None, "mode": "100644"})
                content = git_out(["show", f"{commit_sha}:{new}"])
                blob = api("POST", "git/blobs", {"content": content, "encoding": "utf-8"})
                tree_items.append({"path": new, "sha": blob["sha"], "mode": "100644"})
            else:
                path = parts[-1]
                if parts[0] == "D":
                    tree_items.append({"path": path, "sha": None, "mode": "100644"})
                else:
                    content = git_out(["show", f"{commit_sha}:{path}"])
                    blob = api("POST", "git/blobs", {"content": content, "encoding": "utf-8"})
                    tree_items.append({"path": path, "sha": blob["sha"], "mode": "100644"})
        if not tree_items:
            print("无变更？"); sys.exit(1)

        new_tree = api("POST", "git/trees", {"base_tree": cur_tree, "tree": tree_items})
        new_commit = api("POST", "git/commits",
                         {"message": msg, "tree": new_tree["sha"], "parents": [cur_head]})
        api("PATCH", "git/refs/heads/main", {"sha": new_commit["sha"], "force": False})
        print(f"pushed -> {new_commit['sha'][:8]}")

if __name__ == "__main__":
    main()
