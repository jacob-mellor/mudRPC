"""The PyPI wheel, written with the standard library: mud.py, mud_grpc.py and the library beside them, SKILL.md with the
metadata, and the RECORD a wheel carries (PEP 427). wheel.py <version> <bindings/python> <dist> <out>; the platform tag
is this machine's, the one the library in <dist> was built for."""
import base64
import csv
import hashlib
import io
import os
import platform
import sys
import zipfile

version, src, dist, out = sys.argv[1:5]
lib = next(f for f in os.listdir(dist) if f in ("libmudrpc.dylib", "libmudrpc.so", "mudrpc.dll"))
arch = {"x86_64": "x86_64", "AMD64": "x86_64", "arm64": "arm64", "aarch64": "aarch64"}[platform.machine()]
tag = {"Darwin": f"macosx_11_0_{arch}", "Linux": f"manylinux_2_28_{arch}", "Windows": "win_amd64"}[platform.system()]   # Linux: built on glibc 2.28 or older, or the tag lies
info = f"mudrpc-{version}.dist-info"
readme = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "README.md"), encoding="utf-8").read()
metadata = f"""Metadata-Version: 2.1
Name: mudrpc
Version: {version}
Summary: Mud RPC: gRPC for unstable networks. gRPC's service model over mesh, radio and UDP as sealed packets, without HTTP/2 or a server stack, and a grpcio channel and server so the stubs grpcio-tools generates run unchanged.
Home-page: https://github.com/jacob-mellor/mudRPC
License: MIT
Keywords: grpc,rpc,grpcio,protobuf,mesh,udp,delay-tolerant,iot,drone,lora,no-http2
Classifier: License :: OSI Approved :: MIT License
Classifier: Programming Language :: Python :: 3
Classifier: Topic :: System :: Networking
Requires-Python: >=3.9
Description-Content-Type: text/markdown

{readme}"""
wheel = f"Wheel-Version: 1.0\nGenerator: mudrpc\nRoot-Is-Purelib: false\nTag: py3-none-{tag}\n"
files = [("mud.py", open(os.path.join(src, "mud.py"), "rb").read()), ("mud_grpc.py", open(os.path.join(src, "mud_grpc.py"), "rb").read()),
         (lib, open(os.path.join(dist, lib), "rb").read()), (f"{info}/SKILL.md", open(os.path.join(src, "SKILL.md"), "rb").read()),
         (f"{info}/METADATA", metadata.encode()), (f"{info}/WHEEL", wheel.encode()), (f"{info}/top_level.txt", b"mud\nmud_grpc\n")]
record = io.StringIO()
rows = csv.writer(record, lineterminator="\n")
for name, data in files:
    rows.writerow([name, "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode(), len(data)])
rows.writerow([f"{info}/RECORD", "", ""])
os.makedirs(out, exist_ok=True)
with zipfile.ZipFile(os.path.join(out, f"mudrpc-{version}-py3-none-{tag}.whl"), "w", zipfile.ZIP_DEFLATED) as z:
    for name, data in files + [(f"{info}/RECORD", record.getvalue().encode())]:
        z.writestr(zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0)), data)   # one date, so two builds give one file
