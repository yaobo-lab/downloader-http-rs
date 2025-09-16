use reqwest::header::HeaderMap;
use url::Url;
pub struct Config {
    // pub filename: String,
    pub save_dir: String,
    pub file_md5: String,
    pub url: Option<Url>,
    pub headers: HeaderMap,
    pub content_len: u64,
    pub download_len: u64,
    pub progress: u8,
    pub chunk_size: u64,
    pub timeout: u64,
    pub num_workers: usize,
    pub max_retries: u8,
    //debug 模式
    pub debug: bool,
    //下载进度回调
    pub on_down_progress: Option<Box<dyn Fn(u8) + Send + Sync + 'static>>,
    //下载完成回调
    pub on_down_finish: Option<fn(String)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: None,
            save_dir: "".to_string(),
            //  filename: "".to_string(),
            file_md5: "".to_string(),
            headers: HeaderMap::new(),
            timeout: 0,
            max_retries: 3,
            num_workers: 2,
            //2MB
            chunk_size: 2097152,
            debug: false,
            content_len: 0,
            download_len: 0,
            progress: 0,
            on_down_progress: None,
            on_down_finish: None,
        }
    }
}
