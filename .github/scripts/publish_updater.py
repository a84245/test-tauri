#!/usr/bin/env python3
"""pengmaitw updater 产物发布辅助脚本（S3/MinIO）。

用法（由 .github/workflows/release.yml 调用）：
  python publish_updater.py upload <os> <version> <bundle_dir>   # 每个构建矩阵 job
  python publish_updater.py manifest <version> <fragments_dir>   # 汇总片段生成 latest.json

环境变量：
  MINIO_ENDPOINT      MinIO S3 endpoint，如 https://minio.example.com（path-style）
  MINIO_ACCESS_KEY    Access Key
  MINIO_SECRET_KEY    Secret Key
  MINIO_BUCKET        桶名
  PUBLIC_BASE_URL     客户端公网可访问的 http(s) 前缀，形如 https://<host>/<bucket>
  MINIO_UPLOAD_PREFIX 桶内前缀，默认 pengmaitw/updates
"""
import datetime
import json
import os
import pathlib
import sys

PRODUCT = "pengmaitw"
DEFAULT_PREFIX = f"{PRODUCT}/updates"

# 每平台优先选择的 updater 安装包类型
_PREFERRED = {
    "windows": (".exe",),                       # NSIS
    "macos": (".app.tar.gz", ".app"),           # 官方要求 .app.tar.gz 或 .app
    "linux": (".AppImage", ".deb"),             # AppImage 免 root，优先
}


def _fail(msg: str):
    print(f"::error::{msg}", file=sys.stderr)
    sys.exit(1)


def _s3():
    import boto3
    from botocore.config import Config
    return boto3.client(
        "s3",
        endpoint_url=os.environ["MINIO_ENDPOINT"],
        aws_access_key_id=os.environ["MINIO_ACCESS_KEY"],
        aws_secret_access_key=os.environ["MINIO_SECRET_KEY"],
        region_name=os.environ.get("MINIO_REGION", "us-east-1"),
        config=Config(s3={"addressing_style": "path"}),
    )


def _public_url(key: str) -> str:
    base = os.environ.get("PUBLIC_BASE_URL")
    if base:
        return f"{base.rstrip('/')}/{key}"
    # 未单独配置公开前缀时，默认 path-style: endpoint/<bucket>/<key>
    endpoint = os.environ["MINIO_ENDPOINT"].rstrip("/")
    bucket = os.environ["MINIO_BUCKET"]
    return f"{endpoint}/{bucket}/{key}"


def _bucket_prefix() -> tuple:
    return os.environ["MINIO_BUCKET"], os.environ.get("MINIO_UPLOAD_PREFIX", DEFAULT_PREFIX).strip("/")


def _target_for(os_name: str) -> str:
    # 与 tauri-plugin-updater 的 Updater::target() 一致：{os}-{arch}
    arch = "x86_64"
    if os_name == "macos":
        return "darwin-aarch64"  # CI 只构建 Apple Silicon
    if os_name == "linux":
        return "linux-x86_64"
    if os_name == "windows":
        return "windows-x86_64"
    _fail(f"未知平台: {os_name}")


def _find_artifact(bundle_dir: str, os_name: str):
    """在 bundle 目录里找 updater 产物：存在同名 .sig 的安装包。

    返回 (bin_path, sig_path)。优先取 _PREFERRED 里的类型。
    """
    bundle = pathlib.Path(bundle_dir)
    candidates = []
    for sig in bundle.rglob("*.sig"):
        # "foo-setup.exe.sig" -> 去掉 ".sig" 得安装包路径
        bin_path = pathlib.Path(str(sig)[: -len(".sig")])
        if bin_path.is_file():
            candidates.append((bin_path, sig))
    if not candidates:
        _fail(f"在 {bundle} 下未找到带 .sig 的 updater 产物")

    prefs = _PREFERRED.get(os_name, [])
    for suffix in prefs:
        for bin_path, sig in candidates:
            if bin_path.name.endswith(suffix):
                return bin_path, sig
    # 没匹配到优先类型，退回第一个（保持确定性：按路径排序）
    return sorted(candidates, key=lambda c: str(c[0]))[0]


def _cmd_upload(os_name: str, version: str, bundle_dir: str):
    client = _s3()
    bucket, prefix = _bucket_prefix()
    target = _target_for(os_name)

    bin_path, sig_path = _find_artifact(bundle_dir, os_name)
    bin_key = f"{prefix}/{version}/{bin_path.name}"
    sig_key = f"{bin_key}.sig"

    print(f"上传安装包  -> s3://{bucket}/{bin_key}")
    client.upload_file(str(bin_path), bucket, bin_key,
                       ExtraArgs={"ContentType": "application/octet-stream"})
    client.upload_file(str(sig_path), bucket, sig_key,
                       ExtraArgs={"ContentType": "text/plain"})

    signature = sig_path.read_text(encoding="utf-8").strip()
    fragment = {
        target: {
            "url": _public_url(bin_key),
            "signature": signature,
        }
    }
    out_dir = pathlib.Path("fragments")
    out_dir.mkdir(exist_ok=True)
    out_path = out_dir / f"{target}.json"
    out_path.write_text(json.dumps(fragment, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"片段已写入 {out_path}")
    print(json.dumps(fragment, ensure_ascii=False, indent=2))


def _cmd_manifest(version: str, fragments_dir: str):
    platforms = {}
    frag_dir = pathlib.Path(fragments_dir)
    files = sorted(frag_dir.glob("**/*.json")) if frag_dir.is_dir() else []
    if not files:
        _fail(f"未在 {fragments_dir} 找到任何平台片段")
    for f in files:
        platforms.update(json.loads(f.read_text(encoding="utf-8")))

    manifest = {
        "version": version,
        "notes": "",
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }
    body = json.dumps(manifest, ensure_ascii=False, indent=2)
    print("== latest.json ==")
    print(body)

    client = _s3()
    bucket, prefix = _bucket_prefix()
    key = f"{prefix}/latest.json"
    client.put_object(Bucket=bucket, Key=key,
                      Body=body.encode("utf-8"),
                      ContentType="application/json")
    print(f"已上传 -> s3://{bucket}/{key}")


def main():
    if len(sys.argv) < 2:
        _fail(__doc__)
    cmd = sys.argv[1]
    if cmd == "upload":
        if len(sys.argv) != 5:
            _fail("upload 需要 3 个参数: <os> <version> <bundle_dir>")
        _cmd_upload(sys.argv[2], sys.argv[3], sys.argv[4])
    elif cmd == "manifest":
        if len(sys.argv) != 4:
            _fail("manifest 需要 2 个参数: <version> <fragments_dir>")
        _cmd_manifest(sys.argv[2], sys.argv[3])
    else:
        _fail(f"未知子命令: {cmd}")


if __name__ == "__main__":
    main()
