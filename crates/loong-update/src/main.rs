//! `loong-update`：Loong OS 的 A/B 系统更新命令行。说明见 `docs/api/loong-update.md`。

use std::{
    cell::Cell,
    fs::{self, File},
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use loong_update::{
    ApplyOptions, BootBackend, Error, ErrorKind, Observer, Phase, SlotState, Source, System,
    bundle::{self, PackInfo, PackItem},
    ops, sign,
};

const USAGE: &str = "\
用法：
  loong-update status [--boot-state]            查看两个 slot 的状态（--boot-state 只输出本次启动 good/trial）
  loong-update apply [--allow-downgrade] <更新包|URL|->
                                                安装到非活动 slot，下次启动时试运行
  loong-update verify [--key <公钥>]... <更新包|URL|->
                                                验签并完整校验，不写入（默认用本机信任的公钥）
  loong-update mark-good                        确认本次启动成功（loong-boot-ok 开机时调用）
  loong-update rollback [--force]               下次启动改用另一个 slot
开发机：
  loong-update keygen <私钥.pem> <公钥.pub>       生成 ed25519 签名密钥
  loong-update pack --key <私钥.pem> --version <版本> --profile <机型> --arch <架构>
                    --backend uefi-sdboot|android-abl [--min-version <版本>] [--root-fs <类型>]
                    -o <输出.loongup> <种类[@slot]=文件>...
                                                打包并签名，例如 rootfs=root.erofs uki@a=loong-a.efi
环境变量：LOONG_UPDATE_ROOT（测试用的根目录，默认 /）";

fn usage_error(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("loong-update: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}

/// 命令行参数：选项与位置参数。
struct Args {
    rest: Vec<String>,
}

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        let before = self.rest.len();
        self.rest.retain(|a| a != name);
        self.rest.len() != before
    }

    fn values(&mut self, names: &[&str]) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.rest.len() {
            if names.contains(&self.rest[i].as_str()) {
                if i + 1 >= self.rest.len() {
                    return Err(format!("{} 需要参数", self.rest[i]));
                }
                out.push(self.rest.remove(i + 1));
                self.rest.remove(i);
            } else {
                i += 1;
            }
        }
        Ok(out)
    }

    fn value(&mut self, names: &[&str]) -> Result<Option<String>, String> {
        let mut v = self.values(names)?;
        if v.len() > 1 {
            return Err(format!("{} 只能给一次", names[0]));
        }
        Ok(v.pop())
    }

    fn positional(&self) -> Result<&[String], String> {
        if let Some(opt) = self.rest.iter().find(|a| a.starts_with("--")) {
            return Err(format!("未知选项 {opt}"));
        }
        Ok(&self.rest)
    }
}

/// 命令行上的安装进度：终端里原地刷新，否则每 10% 一行（串口日志）。
struct Cli {
    tty: bool,
    last: Cell<i64>,
}

impl Observer for Cli {
    fn phase(&self, phase: Phase) {
        self.last.set(-1);
        if self.tty {
            eprintln!();
        }
        eprintln!("==> {}", phase.describe());
    }

    fn progress(&self, fraction: f64) {
        let pct = (fraction * 100.0).floor() as i64;
        if self.tty {
            if pct != self.last.get() {
                eprint!("\r    {pct:3}%");
                let _ = std::io::stderr().flush();
            }
        } else if pct / 10 != self.last.get() / 10 || self.last.get() < 0 {
            eprintln!("    {pct:3}%");
        }
        self.last.set(pct);
    }
}

fn describe_state(s: SlotState) -> String {
    match s {
        SlotState::Good => "已确认".into(),
        SlotState::Trial {
            tries_left: Some(n),
        } => format!("待确认（还可尝试 {n} 次）"),
        SlotState::Trial { tries_left: None } => "待确认".into(),
        SlotState::Bad => "坏的（不会再自动启动）".into(),
        SlotState::Missing => "没有启动镜像".into(),
    }
}

fn cmd_status(mut args: Args) -> Result<(), Error> {
    let boot_only = args.flag("--boot-state");
    if !args
        .positional()
        .map_err(|e| Error::new(ErrorKind::Failed, e))?
        .is_empty()
    {
        return Err(Error::new(ErrorKind::Failed, "status 不需要参数"));
    }
    let sys = System::from_env()?;
    let st = ops::status(&sys)?;
    if boot_only {
        println!("{}", st.boot.as_str());
        return Ok(());
    }
    println!("机型:       {}（{}，{}）", st.profile, st.arch, st.backend);
    println!(
        "当前:       slot {}，Loong OS {}，本次启动{}",
        st.current,
        st.version,
        match st.boot {
            loong_update::BootState::Good => "已确认",
            loong_update::BootState::Trial => "待确认（试运行）",
        }
    );
    for info in &st.slots {
        let mark = if info.slot == st.current { "*" } else { " " };
        println!(
            "slot {}{mark}     {:<12} {}{}",
            info.slot,
            info.version.as_deref().unwrap_or("?"),
            describe_state(info.state),
            if info.detail.is_empty() {
                String::new()
            } else {
                format!("  [{}]", info.detail)
            }
        );
    }
    match st.next_boot {
        Some(s) => println!("下次启动:   slot {s}"),
        None => println!("下次启动:   未知"),
    }
    Ok(())
}

fn cmd_apply(mut args: Args) -> Result<(), Error> {
    let allow_downgrade = args.flag("--allow-downgrade");
    let pos = args
        .positional()
        .map_err(|e| Error::new(ErrorKind::Failed, e))?;
    let [src] = pos else {
        return Err(Error::new(ErrorKind::Failed, "apply 需要一个更新包"));
    };
    let source = Source::parse(src);
    let sys = System::from_env()?;
    let cli = Cli {
        tty: std::io::stderr().is_terminal(),
        last: Cell::new(-1),
    };
    eprintln!("loong-update: 安装 {source}");
    let applied = ops::apply(&sys, &source, ApplyOptions { allow_downgrade }, &cli);
    if cli.tty {
        eprintln!();
    }
    let applied = applied?;
    println!(
        "已把 Loong OS {} 写入 slot {}（签名：{}）。重启后试运行，系统界面正常启动后自动确认；\
         否则回到当前系统。",
        applied.version, applied.slot, applied.signer
    );
    Ok(())
}

fn cmd_verify(mut args: Args) -> Result<(), Error> {
    let keys = args
        .values(&["--key", "-k"])
        .map_err(|e| Error::new(ErrorKind::Failed, e))?;
    let pos = args
        .positional()
        .map_err(|e| Error::new(ErrorKind::Failed, e))?;
    let [src] = pos else {
        return Err(Error::new(ErrorKind::Failed, "verify 需要一个更新包"));
    };
    let source = Source::parse(src);
    let sys = System::from_env().ok();
    let keyring = if keys.is_empty() {
        sys.as_ref()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotSupported,
                    "本机没有更新配置：请用 --key 指定公钥",
                )
            })?
            .keyring()?
    } else {
        let mut ring = sign::Keyring::new();
        for k in &keys {
            ring.add_file(Path::new(k))?;
        }
        ring
    };
    let (m, signer) = ops::verify(&keyring, &source, &bundle::NoProgress)?;
    println!("签名有效（{signer}），所有载荷的 SHA-256 一致。");
    print!("{}", m.to_toml());
    if let Some(sys) = sys {
        match sys
            .current_version()
            .and_then(|v| ops::check_compat(&sys, &m, &v, false))
        {
            Ok(()) => println!("# 可以安装到本机"),
            Err(e) => println!("# 不能安装到本机：{e}"),
        }
    }
    Ok(())
}

fn cmd_mark_good(args: Args) -> Result<(), Error> {
    if !args.rest.is_empty() {
        return Err(Error::new(ErrorKind::Failed, "mark-good 不需要参数"));
    }
    let sys = System::from_env()?;
    if ops::mark_good(&sys)? {
        println!("已确认本次启动（slot {}）", sys.current_slot()?);
    } else {
        println!("本次启动已经确认过，或不需要确认");
    }
    Ok(())
}

fn cmd_rollback(mut args: Args) -> Result<(), Error> {
    let force = args.flag("--force");
    if !args
        .positional()
        .map_err(|e| Error::new(ErrorKind::Failed, e))?
        .is_empty()
    {
        return Err(Error::new(ErrorKind::Failed, "rollback 不需要参数"));
    }
    let sys = System::from_env()?;
    let slot = ops::rollback(&sys, force)?;
    println!("下次启动将使用 slot {slot}。重启后生效。");
    Ok(())
}

fn write_new(path: &Path, content: &str, mode: u32) -> Result<(), Error> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let mut f = File::options()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| Error::new(ErrorKind::Io, format!("{}: {e}", path.display())))?;
    f.write_all(content.as_bytes())?;
    Ok(())
}

fn cmd_keygen(args: Args) -> Result<(), Error> {
    let [secret, public] = args.rest.as_slice() else {
        return Err(Error::new(
            ErrorKind::Failed,
            "keygen 需要私钥和公钥的输出路径",
        ));
    };
    let key = sign::generate()?;
    write_new(Path::new(secret), &sign::secret_pem(&key), 0o600)?;
    write_new(
        Path::new(public),
        &sign::public_pem(&key.verifying_key()),
        0o644,
    )?;
    println!(
        "已生成密钥 {}（指纹 {}）",
        secret,
        sign::fingerprint(&key.verifying_key())
    );
    Ok(())
}

fn cmd_pack(mut args: Args) -> Result<(), Error> {
    let fail = |e: String| Error::new(ErrorKind::Failed, e);
    let need = |v: Option<String>, name: &str| v.ok_or_else(|| fail(format!("pack 需要 {name}")));
    let key = need(args.value(&["--key", "-k"]).map_err(fail)?, "--key")?;
    let version = need(args.value(&["--version"]).map_err(fail)?, "--version")?;
    let profile = need(args.value(&["--profile"]).map_err(fail)?, "--profile")?;
    let arch = need(args.value(&["--arch"]).map_err(fail)?, "--arch")?;
    let backend = need(args.value(&["--backend"]).map_err(fail)?, "--backend")?;
    let out = need(args.value(&["-o", "--output"]).map_err(fail)?, "-o")?;
    let min_version = args.value(&["--min-version"]).map_err(fail)?;
    let root_fs = args.value(&["--root-fs"]).map_err(fail)?;
    let boot_backend =
        BootBackend::parse(&backend).ok_or_else(|| fail(format!("未知的启动后端 {backend}")))?;
    let items = args
        .positional()
        .map_err(fail)?
        .iter()
        .map(|s| PackItem::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let secret = sign::parse_secret_pem(
        &fs::read_to_string(&key).map_err(|e| fail(format!("{key}: {e}")))?,
    )?;
    // 可复现构建：SOURCE_DATE_EPOCH
    let created = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        });
    let info = PackInfo {
        version,
        profile,
        arch,
        boot_backend,
        min_version,
        root_fs,
        created,
    };
    let out = PathBuf::from(out);
    let tmp = out.with_extension("loongup.tmp");
    let result = (|| {
        let mut f = std::io::BufWriter::with_capacity(bundle::CHUNK, File::create(&tmp)?);
        let m = bundle::pack(&info, &items, &secret, &mut f)?;
        f.into_inner()
            .map_err(|e| Error::from(e.into_error()))?
            .sync_all()?;
        fs::rename(&tmp, &out)?;
        Ok::<_, Error>(m)
    })();
    let m = match result {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    println!(
        "已生成 {}：Loong OS {}（{}，{}），{} 个载荷，共 {} MiB，签名公钥指纹 {}",
        out.display(),
        m.version,
        m.profile,
        m.boot_backend,
        m.payloads.len(),
        m.total_size() >> 20,
        sign::fingerprint(&secret.verifying_key())
    );
    Ok(())
}

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1);
    let Some(cmd) = argv.next() else {
        return usage_error("缺少命令");
    };
    let args = Args {
        rest: argv.collect(),
    };
    let result = match cmd.as_str() {
        "status" => cmd_status(args),
        "apply" => cmd_apply(args),
        "verify" => cmd_verify(args),
        "mark-good" => cmd_mark_good(args),
        "rollback" => cmd_rollback(args),
        "keygen" => cmd_keygen(args),
        "pack" => cmd_pack(args),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "-V" | "--version" => {
            println!("loong-update {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        other => return usage_error(format!("未知命令 {other}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("loong-update: {e}（{}）", e.kind().name());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parsing() {
        let mut a = Args {
            rest: ["--key", "k", "x", "--force", "-k", "j", "y"]
                .map(String::from)
                .to_vec(),
        };
        assert!(a.flag("--force"));
        assert!(!a.flag("--force"));
        assert_eq!(a.values(&["--key", "-k"]).unwrap(), ["k", "j"]);
        assert_eq!(a.positional().unwrap(), ["x", "y"]);
        let mut b = Args {
            rest: vec!["--version".into()],
        };
        assert!(b.value(&["--version"]).is_err());
        let c = Args {
            rest: vec!["--bogus".into()],
        };
        assert!(c.positional().is_err());
    }
}
