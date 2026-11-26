mod bluetooth;

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use xbloom_device::{Studio, StudioRecipe};
use xbloom_recipes::{Client, HttpTransport, Recipe, RecipeSource, Region};

#[derive(Parser)]
#[command(version, about = "导出 xBloom 账号配方并控制 Studio 咖啡机")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum CloudRegion {
    Intl,
    Cn,
}

impl From<CloudRegion> for Region {
    fn from(value: CloudRegion) -> Self {
        match value {
            CloudRegion::Intl => Self::International,
            CloudRegion::Cn => Self::China,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ExportSource {
    /// 自建和已保存的分享配方。
    Personal,
    /// App 官方产品配方目录。
    Official,
    /// 个人库及官方产品配方目录。
    All,
}

impl From<ExportSource> for RecipeSource {
    fn from(value: ExportSource) -> Self {
        match value {
            ExportSource::Personal => Self::Personal,
            ExportSource::Official => Self::Official,
            ExportSource::All => Self::All,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// 登录并导出个人或官方产品配方。
    Export {
        #[arg(long, env = "XBLOOM_EMAIL")]
        email: String,
        /// pass 条目名称，使用首行作为 xBloom 密码。
        #[arg(long, value_name = "PASS_ENTRY")]
        password: Option<String>,
        #[arg(long, default_value = "intl")]
        region: CloudRegion,
        #[arg(long, default_value = "personal")]
        source: ExportSource,
        /// 输出文件，省略或使用 - 时输出至 stdout。
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 每行向标准输出写入一条配方 JSON。
        #[arg(long)]
        json_lines: bool,
    },
    /// 读取一条公开分享配方。
    Share {
        url: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// 查找附近的 Studio，输出可用于 --device 的标识。
    Scan,
    /// 连接 Studio 并验证蓝牙协议握手。
    Connect {
        #[arg(long)]
        device: Option<String>,
    },
    /// 加载配方，默认在机身上确认启动。
    Brew {
        /// 单条配方 JSON 或 export 输出的 JSON 数组。
        file: PathBuf,
        /// 当输入是多条配方时，选择配方的 tableId。
        #[arg(long)]
        id: Option<u64>,
        /// scan 显示的设备标识或完整名称。
        #[arg(long)]
        device: Option<String>,
        /// 配方加载成功后从命令行启动冲煮。
        #[arg(long, conflicts_with = "dry_run")]
        start: bool,
        /// 校验并编码配方，不连接机器。
        #[arg(long)]
        dry_run: bool,
    },
    /// 取消 Studio 当前的冲煮。
    Cancel {
        #[arg(long)]
        device: Option<String>,
    },
}

struct Http(reqwest::Client);

impl Http {
    fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        ))
    }
}

impl HttpTransport for Http {
    async fn post(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        let response = self
            .0
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Referer", "https://share-h5.xbloom.com/")
            .header(
                "User-Agent",
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)",
            )
            .body(body)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|error| error.to_string())?
            .to_vec())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Command::Export {
            email,
            password,
            region,
            source,
            output,
            json_lines,
        } => {
            if json_lines && output.as_deref().is_some_and(|path| path != Path::new("-")) {
                bail!("--json-lines 的输出目标须为标准输出");
            }
            let client = Client::new(Http::new()?, region.into());
            let password = read_password(password.as_deref())?;
            let session = client.login(&email, &password).await?;
            drop(password);
            if json_lines {
                let mut stdout = io::stdout().lock();
                client
                    .visit_recipes(&session, source.into(), |recipe| {
                        serde_json::to_writer(&mut stdout, &recipe)?;
                        writeln!(stdout)?;
                        stdout.flush()
                    })
                    .await?;
            } else {
                let recipes = client.recipes(&session, source.into()).await?;
                write_json(&recipes, output.as_deref())?;
                eprintln!("已导出 {} 条配方", recipes.len());
            }
        }
        Command::Share { url, output } => {
            let recipe = Client::new(Http::new()?, Region::International)
                .shared_recipe(&url)
                .await?;
            write_json(&recipe, output.as_deref())?;
        }
        Command::Scan => {
            let devices = bluetooth::scan().await?;
            if devices.is_empty() {
                eprintln!("未发现附近的 xBloom Studio");
            }
            for device in devices {
                println!("{}\t{}", device.id(), device.name);
            }
        }
        Command::Connect { device } => {
            let connection = bluetooth::Connection::open(device.as_deref()).await?;
            let mut studio = Studio::new(connection);
            let result = studio.connect().await;
            let disconnect = studio.into_transport().disconnect().await;
            result?;
            disconnect?;
            eprintln!("设备蓝牙握手成功");
        }
        Command::Brew {
            file,
            id,
            device,
            start,
            dry_run,
        } => {
            let recipe = read_recipe(&file, id)?;
            let encoded = StudioRecipe::try_from(&recipe)?;
            if dry_run {
                eprintln!("配方校验通过：{}", recipe.name().unwrap_or("未命名"));
                return Ok(());
            }

            let connection = bluetooth::Connection::open(device.as_deref()).await?;
            let mut studio = Studio::new(connection);
            let result = async {
                studio.load(&encoded).await?;
                if start {
                    studio.start().await?;
                } else {
                    eprintln!("配方已加载，请在机器上确认启动");
                    studio.wait_for_start(Duration::from_secs(120)).await?;
                }
                eprintln!("机器已开始冲煮");
                Ok::<(), xbloom_device::Error>(())
            }
            .await;
            let disconnect = studio.into_transport().disconnect().await;
            result?;
            disconnect?;
        }
        Command::Cancel { device } => {
            let connection = bluetooth::Connection::open(device.as_deref()).await?;
            let mut studio = Studio::new(connection);
            let result = studio.cancel().await;
            let disconnect = studio.into_transport().disconnect().await;
            result?;
            disconnect?;
            eprintln!("机器已确认取消");
        }
    }
    Ok(())
}

fn read_password(entry: Option<&str>) -> Result<String> {
    let Some(entry) = entry else {
        return match std::env::var("XBLOOM_PASSWORD") {
            Ok(password) => Ok(password),
            Err(std::env::VarError::NotPresent) => Ok(rpassword::prompt_password("xBloom 密码：")?),
            Err(error) => Err(error.into()),
        };
    };

    if entry.trim().is_empty() {
        bail!("--password 需要 pass 条目名称");
    }
    let output = ProcessCommand::new("pass")
        .args(["show", "--", entry])
        .output()
        .context("无法运行 pass")?;
    if !output.status.success() {
        bail!("pass 无法读取条目 `{entry}`（{}）", output.status);
    }
    let contents = String::from_utf8(output.stdout).context("pass 条目需要使用 UTF-8 编码")?;
    let password = contents
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .context("pass 条目的首行为空")?;
    Ok(password.to_owned())
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RecipeInput {
    Single(Recipe),
    Library(Vec<Recipe>),
}

fn read_recipe(path: &Path, id: Option<u64>) -> Result<Recipe> {
    let file = File::open(path).with_context(|| format!("无法读取 {}", path.display()))?;
    let input = serde_json::from_reader(BufReader::new(file)).context("无法解析配方 JSON")?;
    let recipes = match input {
        RecipeInput::Single(recipe) => vec![recipe],
        RecipeInput::Library(recipes) => recipes,
    };
    match id {
        Some(id) => recipes
            .into_iter()
            .find(|recipe| recipe.id() == Some(id))
            .with_context(|| format!("配方文件中没有 tableId={id}")),
        None if recipes.len() == 1 => Ok(recipes.into_iter().next().unwrap()),
        None if recipes.is_empty() => bail!("配方文件为空"),
        None => bail!("配方文件中有多条配方，请使用 --id 选择"),
    }
}

fn write_json(value: &impl Serialize, output: Option<&Path>) -> Result<()> {
    if let Some(path) = output.filter(|path| *path != Path::new("-")) {
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(temp.as_file_mut(), value)?;
        writeln!(temp)?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .with_context(|| format!("无法保存 {}", path.display()))?;
    } else {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer_pretty(&mut stdout, value)?;
        writeln!(stdout)?;
    }
    Ok(())
}
