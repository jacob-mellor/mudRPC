#!/usr/bin/env bash
# Every package from one built library, each then installed into a fresh project that runs its skill's example:
#   packages/build.sh <version> <dist> <out>
# <dist> holds libmudrpc.{a,dylib|so}, mudrpc.wasm and mudrpc.h as ./ci.sh release leaves them; <out> gets crates/, npm/,
# nuget/, pypi/, maven/, conan/ and vcpkg/ with SHA256SUMS, and check/, the fresh projects. The library is this machine's,
# so the native packages carry this platform; a release on each platform adds its own. Publishing is the owner's, with the
# accounts: cargo publish in the order crates/ lists; npm publish; dotnet nuget push; twine upload; the Central portal's
# bundle of maven/; conan upload; the port to the vcpkg registry; for Go, the tag bindings/go/v<version> on the same commit.
# Needs what the drop-ins build with, plus conan and vcpkg (brew), with a vcpkg clone at VCPKG_ROOT, made beside <out> when unset.
set -euo pipefail
version=$1 dist=$(cd "$2" && pwd) out=$(mkdir -p "$3" && cd "$3" && pwd)
here=$(cd "$(dirname "$0")" && pwd) root=$(dirname "$here") b=$root/bindings
lib=$(ls "$dist" | grep -E -m1 '^(libmudrpc\.(dylib|so)|mudrpc\.dll)$')
case "$(uname -s)-$(uname -m)" in Darwin-arm64) rid=osx-arm64 triplet=arm64-osx ;; Darwin-x86_64) rid=osx-x64 triplet=x64-osx ;; Linux-x86_64) rid=linux-x64 triplet=x64-linux ;; Linux-aarch64) rid=linux-arm64 triplet=arm64-linux ;; esac
example() { awk -v l="$1" '$0 == "```" l { on = 1; next } on && /^```/ { exit } on' "$b/$2/SKILL.md"; }   # the skill's first fenced block in that language
want=$'drone: Arm\ngcs: status 0 armed'
check() { [ "$2" = "$want" ] || { echo "$1 ran the skill's example and printed: $2"; exit 1; }; echo "== $1: installed into a fresh project, the skill's example runs"; }
rm -rf "$out"; mkdir -p "$out"/{crates,npm,nuget,pypi,maven,conan,vcpkg/mudrpc,check}

echo "== crates.io: each crate as cargo publish sends it, verified by a build from the .crate; publish in this order"
(cd "$root" && cargo package -q --locked --workspace --target-dir "$out/crates/target")
for c in mudrpc-core mudrpc-auth mudrpc-gcm mudrpc-adapters mudrpc-gen mudrpc-c; do mv "$out"/crates/target/package/$c-*.crate "$out/crates/"; echo "$c"; done
rm -r "$out/crates/target"

echo "== npm"
stage=$out/npm/mudrpc
mkdir -p "$stage"
cp "$b"/js/{mud.js,mud-connect.js,SKILL.md} "$dist/mudrpc.wasm" "$here/README.md" "$stage/"
sed "s/\"version\": \"0.0.0\"/\"version\": \"$version\"/" "$here/npm/package.json" > "$stage/package.json"
(cd "$out/npm" && npm pack ./mudrpc --loglevel silent > /dev/null && rm -r mudrpc)
p=$out/check/npm
mkdir -p "$p/.claude"
echo '{ "name": "check", "private": true, "type": "module" }' > "$p/package.json"
(cd "$p" && npm install --loglevel silent --no-audit --no-fund "$out/npm/mudrpc-$version.tgz")
example js js | sed "s#'./mud.js'#'mudrpc'#" > "$p/example.js"
check npm "$(cd "$p" && node example.js)"
test -f "$p/.claude/skills/mudrpc/SKILL.md"   # the postinstall put the skill where an agent reads skills

echo "== NuGet"
stage=$out/nuget/build
mkdir -p "$stage"
cp "$here/nuget/mudrpc.csproj" "$stage/"
dotnet pack "$stage/mudrpc.csproj" -c Release -nologo -v q -o "$out/nuget" -p:Version="$version" -p:Sources="$b" -p:Dist="$dist" -p:Rid=$rid -p:Lib="$lib" -p:Readme="$here/README.md"
rm -r "$stage"
p=$out/check/nuget
mkdir -p "$p"
echo "<configuration><packageSources><clear /><add key=\"local\" value=\"$out/nuget\" /><add key=\"nuget.org\" value=\"https://api.nuget.org/v3/index.json\" /></packageSources></configuration>" > "$p/nuget.config"
echo "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><Nullable>enable</Nullable><ImplicitUsings>disable</ImplicitUsings></PropertyGroup><ItemGroup><PackageReference Include=\"mudrpc\" Version=\"$version\" /></ItemGroup></Project>" > "$p/check.csproj"
example csharp dotnet > "$p/Program.cs"
check NuGet "$(cd "$p" && NUGET_PACKAGES=$p/packages dotnet run -c Release -nologo -v q)"   # its own package cache: never a stale copy of this version

echo "== PyPI"
python3 -I "$here/pypi/wheel.py" "$version" "$b/python" "$dist" "$out/pypi"
p=$out/check/pypi
python3 -m venv "$p/venv"
"$p/venv/bin/pip" install -q --disable-pip-version-check "$out"/pypi/mudrpc-*.whl
example python python > "$p/example.py"
check PyPI "$(cd "$p" && venv/bin/python -I example.py)"

echo "== Maven Central"
grpc=$root/target/maven/grpc-api-1.85.1.jar sha=159371ce7ecb1de72f910571436f7866baa595c820271fcb930961212b7c6363   # the pin bindings/c/tests/abi.rs holds
[ -f "$grpc" ] || { mkdir -p "$(dirname "$grpc")"; curl -sfL -o "$grpc" https://repo1.maven.org/maven2/io/grpc/grpc-api/1.85.1/grpc-api-1.85.1.jar; }
test "$(shasum -a 256 "$grpc" | cut -c1-64)" = $sha
classes=$out/maven/classes
mkdir -p "$classes"
javac --release 22 -d "$classes" -cp "$grpc" "$b"/java/Mud.java "$b"/java/MudChannel.java
javadoc -quiet -Xdoclint:none -d "$out/maven/javadoc" -cp "$grpc" "$b"/java/Mud.java "$b"/java/MudChannel.java
cp "$dist/$lib" "$b/java/SKILL.md" "$here/README.md" "$classes/"   # the library inside the jar, where Mud.java looks after beside it
jar --create --file "$out/maven/mudrpc-$version.jar" -C "$classes" .
jar --create --file "$out/maven/mudrpc-$version-sources.jar" -C "$b/java" Mud.java -C "$b/java" MudChannel.java
jar --create --file "$out/maven/mudrpc-$version-javadoc.jar" -C "$out/maven/javadoc" .
sed "s/@VERSION@/$version/" "$here/maven/pom.xml" > "$out/maven/mudrpc-$version.pom"
rm -r "$classes" "$out/maven/javadoc"
p=$out/check/maven
mkdir -p "$p"
example java java > "$p/Example.java"
javac --release 22 -cp "$out/maven/mudrpc-$version.jar" -d "$p" "$p/Example.java"
check Maven "$(cd "$p" && java --enable-native-access=ALL-UNNAMED -cp "$out/maven/mudrpc-$version.jar:." Example)"

echo "== Conan: the recipe exported, created from this tree and its test package run"
cp -r "$here/conan" "$out/conan/recipe"   # created from the copy: what conan writes beside a test package stays out of the tree
conan profile detect --exist-ok > /dev/null 2>&1
MUDRPC_TREE=$root conan create "$out/conan/recipe" --version "$version" > "$out/conan/create.log" 2>&1 || { tail -40 "$out/conan/create.log"; exit 1; }
echo "== conan: mudrpc/$version in the local cache; conan upload sends it"

echo "== vcpkg: the port at this commit, installed as an overlay port"
sed "s/@VERSION@/$version/" "$here/vcpkg/mudrpc/vcpkg.json" > "$out/vcpkg/mudrpc/vcpkg.json"
sed "s/@REF@/$(git -C "$root" rev-parse HEAD)/" "$here/vcpkg/mudrpc/portfile.cmake" > "$out/vcpkg/mudrpc/portfile.cmake"
: "${VCPKG_ROOT:=$out/../vcpkg}"
[ -d "$VCPKG_ROOT/.git" ] || git clone -q --depth 1 https://github.com/microsoft/vcpkg "$VCPKG_ROOT"
p=$out/check/vcpkg
mkdir -p "$p/ports/mudrpc"
cp "$out/vcpkg/mudrpc/vcpkg.json" "$p/ports/mudrpc/"
sed "s#https://github.com/jacob-mellor/mudRPC#$root#" "$out/vcpkg/mudrpc/portfile.cmake" > "$p/ports/mudrpc/portfile.cmake"   # this tree's commit, not the published one
VCPKG_ROOT=$VCPKG_ROOT VCPKG_FORCE_SYSTEM_BINARIES=1 vcpkg install mudrpc --overlay-ports="$p/ports" --x-install-root="$p/installed" --x-buildtrees-root="$p/buildtrees" --x-packages-root="$p/packages" > "$p/install.log" 2>&1 || { tail -40 "$p/install.log"; exit 1; }
cc -o "$p/main" "$here/conan/test_package/main.c" -I"$p/installed/$triplet/include" -L"$p/installed/$triplet/lib" -lmudrpc
"$p/main" | grep -q '^mud_size ' && echo "== vcpkg: installed, a C program links and runs"

(cd "$out" && find crates npm nuget pypi maven vcpkg -type f | sort | xargs shasum -a 256 > SHA256SUMS)
echo "== packages: $out"
