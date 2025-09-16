pub mod config;
pub mod http;
use anyhow::{anyhow, Error};
use chksum_md5 as md5;
pub use config::Config;
pub use http::HttpDownload;
use std::fs::File;
use std::path::Path;
//获取文件md5
pub fn get_file_md5(file_path: &str) -> Result<String, Error> {
    if file_path.is_empty() {
        return Err(anyhow!("file_path is empty"));
    }

    let path = Path::new(file_path);

    if !path.exists() {
        return Err(anyhow!("file does not exist"));
    }
    let file = File::open(path)?;
    match md5::chksum(file) {
        Ok(digest) => Ok(digest.to_string()),
        Err(e) => Err(anyhow!("get_file_md5 chksum failed:{}", e)),
    }
}

//解压文件到指定目录
pub fn uzip_file(zipfile: &str, to_dir: &str) -> Result<(), Error> {
    let path = Path::new(&zipfile);
    if !path.exists() {
        return Err(anyhow!("file does not exist"));
    }

    match path.extension() {
        Some(ext) => {
            if !ext.eq_ignore_ascii_case("zip") {
                return Err(anyhow!("file is not zip"));
            }
        }
        _ => {
            return Err(anyhow!("file is not extension"));
        }
    }
    let file = File::open(zipfile)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let to_path = Path::new(to_dir);

    match archive.extract(to_path) {
        Ok(()) => {}
        Err(e) => {
            return Err(anyhow!("extract failed:{}", e));
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
mod test {
    use fast_log::config::Config;
    use fast_log::plugin::file_split::{DateType, KeepType, Rolling, RollingType};
    use fast_log::plugin::packer::LogPacker;
    fn init_log() {
        let cfg = Config::new()
            .level(log::LevelFilter::Trace)
            .file_split(
                "./app.log",
                Rolling::new(RollingType::ByDate(DateType::Day)),
                KeepType::KeepNum(3),
                LogPacker {},
            )
            .console();
        let _ = fast_log::init(cfg);
    }

    #[tokio::test]
    async fn test_auto_download() {
        init_log();

        let filepath = crate::HttpDownload::new()
            .set_max_retries(3)
            .set_num_workers(3)
            .set_file_md5("4f319a7a8cd6d322c6d938f7b8c2adb9".to_owned())
            .debug(true)
            .set_save_dir("./temp/".to_owned())
            .set_url("you url")
            .await
            .unwrap_or_else(|e| {
                println!("程序崩了:{}", e);
                panic!("{}", e);
            })
            .start()
            .await
            .unwrap_or_else(|e| {
                panic!("{}", e);
            });
        println!("download ok-->:{}", filepath);
    }

    //#[test]
    fn test_unzip() {
        // crate::uzip_file("./temp/wsl.zip","./wsl");
        println!("unzip ok");
    }

    //#[tokio::test]
    async fn test_chunk_download() {
        init_log();

        let filepath = crate::HttpDownload::new()
            .set_max_retries(3)
            .set_num_workers(1)
            .set_chunk_size(10485760)
            .debug(true)
            .set_file_md5("1a4f97564f6127ecfc12d19ed39d0b21".to_owned())
            .set_save_dir("./temp/".to_owned())
            .set_url("you download url")
            .await
            .unwrap_or_else(|e| {
                println!("程序崩了:{}", e.to_string());
                ::std::process::exit(1);
            })
            .chunk_download()
            .await
            .unwrap_or_else(|e| {
                panic!("{}", e);
            });

        println!("download ok-->:{}", filepath);
    }

    //  #[tokio::test]
    async fn test_download() {
        init_log();

        let filepath = crate::HttpDownload::new()
            .set_file_md5("1a4f97564f6127ecfc12d19ed39d0b21".to_owned())
            .set_save_dir("./temp/".to_owned())
            .set_url("you download url")
            .await
            .unwrap_or_else(|e| {
                panic!("{}", e);
            })
            .gener_download()
            .await
            .unwrap_or_else(|e| {
                println!("程序崩了:{}", e.to_string());
                ::std::process::exit(1);
            });
        println!("gener_download ok-->:{}", filepath);
    }

    //#[test]
    fn test_file_md5() {
        let file_path = "./temp/CentOS-7-x86_64-DVD-2009.iso";
        let md5 = crate::get_file_md5(file_path).unwrap_or_else(|_| panic!("get_file_md5 failed"));
        //16f8211fb6b447c52c5591fe471005d1
        println!("md5:{}", md5);
    }
}
