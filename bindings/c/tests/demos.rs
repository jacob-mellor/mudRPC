//! The demos in `demos/`, every language's built and run from its folder as its README says, each held to the lines its use
//! case's README prints. End to end: the drop-ins' toolchains, `cargo test -- --include-ignored`.
use std::path::{Path, PathBuf};
use std::process::Command;

const DRONES: &str = "land: FAILED_PRECONDITION not armed\narmed drone-1 for gcs\ntelemetry 0:10m 100% 1:20m 98% 2:30m 96%\nlanded\n";
const SENSORS: &str = "reading sensor-1 21.50C\nreading sensor-2 19.00C\ncalibrated sensor-2 by 0.50C\n";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn lib() -> String {
    let ext = if cfg!(target_os = "macos") { "dylib" } else { "so" };
    format!("{}/target/release/libmudrpc.{ext}", root().display())
}

/// Runs `cmd args` in `demos/<use>/<lang>` with the library named; stdout back, stderr empty, exit 0.
fn run(dir: &str, cmd: &str, args: &[&str], env: &[(&str, &str)]) -> String { go(dir, cmd, args, env, true) }

/// A build step: exit 0, whatever its toolchain says on stderr.
fn build(dir: &str, cmd: &str, args: &[&str], env: &[(&str, &str)]) { go(dir, cmd, args, env, false); }

fn go(dir: &str, cmd: &str, args: &[&str], env: &[(&str, &str)], quiet: bool) -> String {
    let mut c = Command::new(cmd);
    c.args(args).current_dir(root().join("demos").join(dir)).env("MUDRPC_LIB", lib()).env("MUDRPC_WASM", format!("{}/target/wasm32-unknown-unknown/release/mudrpc.wasm", root().display()));
    for (k, v) in env { c.env(k, v); }
    let o = c.output().unwrap();
    assert!(o.status.success() && (!quiet || o.stderr.is_empty()), "{dir}: {cmd} {args:?}\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

fn built() {
    let r = root();
    assert!(Command::new("cargo").args(["build", "--release", "-p", "mudrpc-c", "-p", "mudrpc-gen"]).current_dir(&r).status().unwrap().success());
    assert!(Command::new("cargo").args(["build", "--release", "-p", "mudrpc-c", "--target", "wasm32-unknown-unknown"]).current_dir(&r).status().unwrap().success());
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_dotnet_demos_print_their_lines() {
    built();
    for (dir, want) in [("drones/dotnet", DRONES), ("sensors/dotnet", SENSORS)] {
        assert_eq!(run(dir, "dotnet", &["run", "-c", "Release", "-nologo", "-v", "q"], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_python_demos_print_their_lines() {
    built();
    let venv = root().join("target/demos/python");
    if !venv.join("bin/python").exists() { assert!(Command::new("python3").args(["-m", "venv"]).arg(&venv).status().unwrap().success()); }
    let python = venv.join("bin/python").to_str().unwrap().to_string();
    run("drones/python", &python, &["-I", "-m", "pip", "install", "-q", "--disable-pip-version-check", "--require-hashes", "--only-binary=:all:", "-r", "requirements.txt"], &[]);
    for (dir, proto, want) in [("drones/python", "drone.proto", DRONES), ("sensors/python", "sensor.proto", SENSORS)] {
        std::fs::create_dir_all(root().join("demos").join(dir).join("gen")).unwrap();
        let mut args = vec!["-I", "-m", "grpc_tools.protoc", "-I", "..", "--python_out=gen"];
        if dir.starts_with("drones") { args.push("--grpc_python_out=gen"); }
        args.push(proto);
        run(dir, &python, &args, &[]);
        let program = if dir.starts_with("drones") { "drones.py" } else { "sensors.py" };
        assert_eq!(run(dir, &python, &[program], &[("PYTHONPATH", "../../../bindings/python:gen")]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_javascript_demos_print_their_lines() {
    built();
    for (dir, proto, program, want) in [("drones/js", "drone.proto", "drones.js", DRONES), ("sensors/js", "sensor.proto", "sensors.js", SENSORS)] {
        run(dir, "npm", &["ci", "--loglevel", "silent", "--no-audit", "--no-fund"], &[]);
        let at = root().join("demos").join(dir);
        std::fs::create_dir_all(at.join("gen")).unwrap();
        if dir.starts_with("drones") {
            std::fs::create_dir_all(at.join("vendor")).unwrap();
            for f in ["mud.js", "mud-connect.js"] { std::fs::copy(root().join("bindings/js").join(f), at.join("vendor").join(f)).unwrap(); }
        }
        run(dir, "protoc", &["--plugin=protoc-gen-es=node_modules/.bin/protoc-gen-es", "--es_out=gen", "--es_opt=target=js,import_extension=js", "-I", "..", proto], &[]);
        assert_eq!(run(dir, "node", &[program], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_c_demos_print_their_lines() {
    built();
    let mud = format!("{}/target/release/mud", root().display());
    let extra: &[&str] = if cfg!(target_os = "linux") { &["-lpthread", "-ldl", "-lm"] } else { &[] };
    for (dir, proto, program, want) in [("drones/c", "../drone.proto", "drones", DRONES), ("sensors/c", "../sensor.proto", "sensors", SENSORS)] {
        run(dir, &mud, &["gen", proto, "--lang", "c", "-o", "gen"], &[]);
        let out = format!("gen/{program}");
        run(dir, "cc", &[&["-std=c11", "-Wall", "-Wextra", "-Wpedantic", "-Werror", "-I", "../../../bindings/c", "-I", "gen", &format!("{program}.c"), "../../../target/release/libmudrpc.a", "-o", &out], extra].concat(), &[]);
        assert_eq!(run(dir, &format!("./{out}"), &[], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_php_demos_print_their_lines() {
    built();
    let plugin = String::from_utf8(Command::new("which").arg("grpc_php_plugin").output().unwrap().stdout).unwrap();
    for (dir, proto, program, want) in [("drones/php", "drone.proto", "drones.php", DRONES), ("sensors/php", "sensor.proto", "sensors.php", SENSORS)] {
        run(dir, "composer", &["install", "--quiet", "--no-interaction"], &[]);
        std::fs::create_dir_all(root().join("demos").join(dir).join("gen")).unwrap();
        let mut args = vec!["-I", "..", "--php_out=gen"];
        let grpc = format!("--plugin=protoc-gen-grpc={}", plugin.trim());
        if dir.starts_with("drones") { args.extend(["--grpc_out=gen", &grpc]); }
        args.push(proto);
        run(dir, "protoc", &args, &[]);
        assert_eq!(run(dir, "php", &[program], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_java_demos_print_their_lines() {
    built();
    let r = root();
    assert!(Command::new("cargo").args(["test", "-q", "-p", "mudrpc-c", "--test", "abi", "--", "--include-ignored", "the_java_and_kotlin"]).current_dir(&r).status().unwrap().success());   // the pinned jars and tools into target/maven
    let m = r.join("target/maven");
    let jar = |n: &str| m.join(n).to_str().unwrap().to_string();
    let (protoc, plugin) = if cfg!(target_os = "macos") { (jar("protoc-4.36.2-osx-aarch_64.exe"), jar("protoc-gen-grpc-java-1.85.1-osx-aarch_64.exe")) } else { (jar("protoc-4.36.2-linux-x86_64.exe"), jar("protoc-gen-grpc-java-1.85.1-linux-x86_64.exe")) };
    let jdk = |t: &str| { let home = std::env::var("JAVA_HOME").unwrap_or_else(|_| "/opt/homebrew/opt/openjdk".into()); if Path::new(&home).exists() { format!("{home}/bin/{t}") } else { t.into() } };
    let grpc = ["grpc-api-1.85.1.jar", "grpc-stub-1.85.1.jar", "grpc-protobuf-1.85.1.jar", "grpc-protobuf-lite-1.85.1.jar", "protobuf-java-4.36.2.jar", "guava-33.6.0-android.jar", "failureaccess-1.0.3.jar"].map(jar).join(":");
    let protobuf = jar("protobuf-java-4.36.2.jar");
    for (dir, proto, pkg, program, cp, want) in [("drones/java", "drone.proto", "drones", "Drones", &grpc, DRONES), ("sensors/java", "sensor.proto", "sensors", "Sensors", &protobuf, SENSORS)] {
        std::fs::create_dir_all(r.join("demos").join(dir).join("gen")).unwrap();
        let mut args = vec!["--java_out=gen", "-I", ".."];
        let plug = format!("--plugin=protoc-gen-grpc-java={plugin}");
        if dir.starts_with("drones") { args.extend([plug.as_str(), "--grpc-java_out=@generated=omit:gen"]); }
        args.push(proto);
        run(dir, &protoc, &args, &[]);
        let gen: Vec<String> = std::fs::read_dir(r.join("demos").join(dir).join("gen/demo").join(pkg)).unwrap().map(|e| e.unwrap().path().to_str().unwrap().to_string()).collect();
        let mut sources: Vec<&str> = vec!["-d", "out", "-cp", cp];
        sources.extend(gen.iter().map(String::as_str));
        sources.push("../../../bindings/java/Mud.java");
        if dir.starts_with("drones") { sources.push("../../../bindings/java/MudChannel.java"); }
        let file = format!("{program}.java");
        sources.push(&file);
        run(dir, &jdk("javac"), &sources, &[]);
        assert_eq!(run(dir, &jdk("java"), &["--enable-native-access=ALL-UNNAMED", "-cp", &format!("out:{cp}"), program], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_rust_demos_print_their_lines() {
    for (dir, want) in [("drones/rust", DRONES), ("sensors/rust", SENSORS)] {
        run(dir, "cargo", &["clippy", "-q", "--locked", "--all-targets", "--", "-D", "warnings"], &[]);
        assert_eq!(run(dir, "cargo", &["run", "-q", "--release", "--locked"], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_cpp_demos_print_their_lines() {
    built();
    let pkg = String::from_utf8(Command::new("pkg-config").args(["--cflags", "--libs", "protobuf"]).output().unwrap().stdout).unwrap();
    let pkg: Vec<&str> = pkg.split_whitespace().collect();
    run("drones/cpp", "c++", &[&["-std=c++17", "../../../bindings/cpp/protoc-gen-mud.cc"], &pkg[..], &["-lprotoc", "-o", "protoc-gen-mud"]].concat(), &[]);
    for (dir, proto, program, want) in [("drones/cpp", "drone.proto", "drones", DRONES), ("sensors/cpp", "sensor.proto", "sensors", SENSORS)] {
        std::fs::create_dir_all(root().join("demos").join(dir).join("gen")).unwrap();
        let mut gen = vec!["-I..", "--cpp_out=gen"];
        if dir.starts_with("drones") { gen.extend(["--plugin=protoc-gen-mud=./protoc-gen-mud", "--mud_out=gen"]); }
        gen.push(proto);
        run(dir, "protoc", &gen, &[]);
        let (src, pb) = (format!("{program}.cc"), format!("gen/{}.pb.cc", proto.trim_end_matches(".proto")));
        run(dir, "c++", &[&["-std=c++17", "-Wall", "-Wextra", "-Werror", "-Igen", "-I../../../bindings/cpp", "-I../../../bindings/c", &src, &pb], &pkg[..], &["-L../../../target/release", "-lmudrpc", "-Wl,-rpath,../../../target/release", "-o", program]].concat(), &[]);
        assert_eq!(run(dir, &format!("./{program}"), &[], &[]), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_go_demos_print_their_lines() {
    built();
    let lib = format!("{}/target/release", root().display());
    let env = [("GOTOOLCHAIN", "local"), ("CGO_LDFLAGS", &format!("-L{lib}"))];
    let rpath = format!("-ldflags=-extldflags=-Wl,-rpath,{lib}");   // once, to the final link: in CGO_LDFLAGS it reaches every cgo package and the linker warns of the duplicate
    for (dir, proto, want) in [("drones/go", "drone", DRONES), ("sensors/go", "sensor", SENSORS)] {
        let drones = dir.starts_with("drones");
        let mut plugins = vec!["build", "-o", "bin/", "google.golang.org/protobuf/cmd/protoc-gen-go"];
        if drones { plugins.push("google.golang.org/grpc/cmd/protoc-gen-go-grpc"); }
        run(dir, "go", &plugins, &env);
        std::fs::create_dir_all(root().join("demos").join(dir).join("pb")).unwrap();
        let m = format!("--go_opt=M{proto}.proto=mudrpc.demo/{}/pb;pb", if drones { "drones" } else { "sensors" });
        let mg = m.replace("--go_opt", "--go-grpc_opt");
        let file = format!("{proto}.proto");
        let mut args = vec!["--plugin=protoc-gen-go=bin/protoc-gen-go", "--go_out=pb", "--go_opt=paths=source_relative", &m];
        if drones { args.extend(["--plugin=protoc-gen-go-grpc=bin/protoc-gen-go-grpc", "--go-grpc_out=pb", "--go-grpc_opt=paths=source_relative", &mg]); }
        args.extend(["-I", "..", &file]);
        run(dir, "protoc", &args, &[]);
        run(dir, "go", &["vet", "./..."], &env);
        assert_eq!(run(dir, "go", &["run", &rpath, "."], &env), want, "{dir}");
    }
}

#[test]
#[ignore = "end to end: cargo test -- --include-ignored"]
fn the_swift_demos_print_their_lines() {
    built();
    let lib = format!("{}/target/release", root().display());
    for (dir, proto, target, want) in [("drones/swift", "drone.proto", "Drones", DRONES), ("sensors/swift", "sensor.proto", "Sensors", SENSORS)] {
        let drones = dir.starts_with("drones");
        build(dir, "swift", &["build", "-c", "release", "--product", "protoc-gen-swift"], &[]);
        if drones { build(dir, "swift", &["build", "-c", "release", "--product", "protoc-gen-grpc-swift-2"], &[]); }
        let gen = format!("Sources/{target}/gen");
        std::fs::create_dir_all(root().join("demos").join(dir).join(&gen)).unwrap();
        let (out, grpc) = (format!("--swift_out={gen}"), format!("--grpc-swift-2_out={gen}"));
        let mut args = vec!["--plugin=protoc-gen-swift=.build/release/protoc-gen-swift", &out];
        if drones { args.extend(["--plugin=protoc-gen-grpc-swift-2=.build/release/protoc-gen-grpc-swift-2", &grpc]); }
        args.extend(["-I", "..", proto]);
        run(dir, "protoc", &args, &[]);
        build(dir, "swift", &["build", "-Xlinker", &format!("-L{lib}"), "-Xlinker", "-rpath", "-Xlinker", &lib], &[]);
        assert_eq!(run(dir, &format!(".build/debug/{target}"), &[], &[]), want, "{dir}");
    }
}
