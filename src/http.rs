use crate::config::Config;
use anyhow::{anyhow, Error};
use bounded_join_set::JoinSet;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Client, Request};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::{sync::mpsc, time::timeout};
use url::{ParseError, Url};

pub struct HttpDownload {
    //配置
    conf: Config,
    //文件名
    filename: String,
    //
    retries: u8,
    //是否支持分片下载
    support_chunk: bool,
    //http client
    httpclient: Client,
    //下载文件句柄
    file: Option<BufWriter<fs::File>>,
    //状态文件句柄
    st_file: Option<BufWriter<fs::File>>,
}

impl fmt::Debug for HttpDownload {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "http download url: {}",
            self.conf.url.as_ref().map(|url| url.as_str()).unwrap_or("")
        )
    }
}

impl HttpDownload {
    pub fn new() -> Self {
        Self {
            conf: Config::default(),
            retries: 0,
            filename: "".to_string(),
            st_file: None,
            file: None,
            support_chunk: false,
            httpclient: Client::new(),
        }
    }

    pub fn new_with_config(conf: Config) -> Self {
        Self {
            conf,
            retries: 0,
            filename: "".to_string(),
            st_file: None,
            file: None,
            support_chunk: false,
            httpclient: Client::new(),
        }
    }

    pub fn set_timeout(&mut self, timeout: u64) -> &mut Self {
        self.conf.timeout = timeout;
        self
    }

    pub fn on_down_progress(
        &mut self,
        cb: Option<Box<dyn Fn(u8) + Send + Sync + 'static>>,
    ) -> &mut Self {
        self.conf.on_down_progress = cb;
        self
    }
    pub fn on_down_finish(&mut self, cb: Option<fn(String)>) -> &mut Self {
        self.conf.on_down_finish = cb;
        self
    }

    pub fn set_headers(&mut self, headers: HeaderMap) -> &mut Self {
        self.conf.headers = headers;
        self
    }
    //保存目录
    pub fn set_save_dir(&mut self, save_dir: String) -> &mut Self {
        self.conf.save_dir = save_dir;
        self
    }
    pub fn set_file_md5(&mut self, md5: String) -> &mut Self {
        self.conf.file_md5 = md5;
        self
    }

    pub fn set_chunk_size(&mut self, chunk_size: u64) -> &mut Self {
        self.conf.chunk_size = chunk_size;
        self
    }
    pub fn set_max_retries(&mut self, max_retries: u8) -> &mut Self {
        self.conf.max_retries = max_retries;
        self
    }
    pub fn set_user_agent(&mut self, user_agent: &str) -> &mut Self {
        if let Ok(ag) = header::HeaderValue::from_str(user_agent) {
            self.conf.headers.insert(header::USER_AGENT, ag);
        }
        self
    }
    pub fn set_num_workers(&mut self, num_workers: usize) -> &mut Self {
        self.conf.num_workers = num_workers;
        self
    }
    pub fn debug(&mut self, debug: bool) -> &mut Self {
        self.conf.debug = debug;
        self
    }

    pub async fn url_headers_info(&self, url: &str) -> Result<(), Error> {
        let url = self.parse_url(url)?;
        let headers = self.get_headers_from_url(&url).await?;
        self.print_headers(&headers);
        Ok(())
    }

    fn print_headers(&self, headers: &HeaderMap) {
        for (hdr, val) in headers.iter() {
            println!("{}: {}", hdr.as_str(), val.to_str().unwrap_or("<..>"));
        }
    }

    //从url 服务中获取headers 信息
    async fn get_headers_from_url(&self, url: &Url) -> Result<HeaderMap, Error> {
        let resp = Client::new()
            .get(url.as_ref())
            .timeout(Duration::from_secs(10))
            .headers(self.conf.headers.clone())
            .header(header::ACCEPT, HeaderValue::from_str("*/*")?);

        let resp = resp.send().await?;
        Ok(resp.headers().clone())
    }

    //检查文件是否已下载过
    fn compari_file_md5(&self, file_path: &str, md5: &str) -> bool {
        if md5.is_empty() {
            return false;
        }

        if let Ok(file_md5) = crate::get_file_md5(&file_path) {
            log::debug!("file_md5:{},md5:{}", file_md5, md5);
            return file_md5.to_lowercase().eq(&md5.to_lowercase());
        }

        false
    }

    fn parse_url(&self, url: &str) -> Result<Url, ParseError> {
        match Url::parse(url) {
            Ok(url) => Ok(url),
            Err(ParseError::RelativeUrlWithoutBase) => {
                let url_with_base = format!("{}{}", "http://", url);
                Url::parse(url_with_base.as_str())
            }
            Err(error) => Err(error),
        }
    }

    //设置下载url
    pub async fn set_url(&mut self, url: &str) -> Result<&mut Self, Error> {
        let url = self
            .parse_url(url)
            .map_err(|e| anyhow!("url不合法: {} err:{}", url, e))?;

        let headers = self.get_headers_from_url(&url).await?;
        //打印http头
        if self.conf.debug {
            self.print_headers(&headers);
        }

        //获下载文件名
        let fname = gen_filename(&url, Some(&headers));
        if fname.is_empty() {
            return Err(anyhow!("filename is empty"));
        }
        self.filename = fname;

        //判断服务器是否支持分片下载
        let server_acccept_ranges = match headers.get(header::ACCEPT_RANGES) {
            Some(val) => val == "bytes",
            None => false,
        };

        self.support_chunk = server_acccept_ranges;

        //文件长度
        let mut content_len = 0;
        if let Some(val) = headers.get(header::CONTENT_LENGTH) {
            content_len = val.to_str().unwrap_or("").parse::<u64>()?;
        }
        self.conf.content_len = content_len;

        self.conf.url = Some(url);

        Ok(self)
    }

    fn on_progress(&mut self) {
        let mut pro = if self.conf.content_len > 0 {
            if self.conf.download_len >= self.conf.content_len {
                100
            } else {
                let r = (self.conf.download_len as f64 / self.conf.content_len as f64) * 100.0;
                r.ceil() as u8
            }
        } else {
            0
        };

        if pro > 100 {
            pro = 100;
        }

        if self.conf.progress != pro {
            self.conf.progress = pro;
            if let Some(evt) = &self.conf.on_down_progress {
                evt(pro);
            }
            log::trace!("download progress :{}", pro);
        }
    }
    fn on_finish(&self, file_path: String) {
        if let Some(evt) = &self.conf.on_down_progress {
            evt(100);
        }
        if let Some(evt) = &self.conf.on_down_finish {
            evt(file_path);
        }
        let filepath = self.get_file_path(format!("{}.st", self.filename));
        let _ = fs::remove_file(&filepath);
    }

    fn write_content(&mut self, content: &[u8]) -> Result<(), Error> {
        if self.file.is_none() {
            return Err(anyhow!("file handler is none"));
        }

        //写入文件字节
        if let Some(ref mut file) = self.file {
            match file.write_all(content) {
                Ok(()) => {}
                Err(e) => {
                    return Err(anyhow!("write_content file.write_all err {}", e));
                }
            }
        }

        self.conf.download_len += content.len() as u64;
        //设置进度条
        self.on_progress();
        Ok(())
    }

    fn chunk_write_content(&mut self, content: (u64, u64, &[u8])) -> Result<(), Error> {
        if self.file.is_none() {
            return Err(anyhow!("file handler is none"));
        }

        let (byte_count, offset, buf) = content;
        //写入文件
        if let Some(ref mut file) = self.file {
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(buf)?;
            file.flush()?;
        }

        //记录进度临时文件
        if let Some(ref mut file) = self.st_file {
            writeln!(file, "{}:{}", byte_count, offset)?;
            file.flush()?;
        }

        self.conf.download_len += byte_count;
        //设置进度条
        self.on_progress();
        Ok(())
    }

    pub async fn start(&mut self) -> Result<String, Error> {
        if self.support_chunk {
            log::info!("use chunk download..");
            self.chunk_download().await
        } else {
            log::info!("use general download..");
            self.gener_download().await
        }
    }

    //普通下载
    #[allow(unused)]
    pub async fn gener_download(&mut self) -> Result<String, Error> {
        let filepath = self.get_file_path(self.filename.clone());

        if self.compari_file_md5(&filepath, &self.conf.file_md5) {
            self.on_finish(filepath.clone());
            return Ok(filepath);
        }

        //不是分片下载不支持，文件续传
        self.file = Some(create_filehandler(
            &self.filename,
            &self.conf.save_dir,
            false,
        )?);

        let timeout = self.conf.timeout;

        let headers = self.conf.headers.clone();

        let Some(url) = self.conf.url.as_ref() else {
            return Err(anyhow!("url is empty"));
        };

        let mut req = self.httpclient.get(url.clone());
        if timeout > 0 {
            req = req.timeout(Duration::from_secs(timeout));
        }
        let req = req.headers(headers).build()?;

        let mut resp = self.httpclient.execute(req).await?;

        let ct_len = if let Some(val) = resp.headers().get(header::CONTENT_LENGTH) {
            Some(val.to_str()?.parse::<usize>()?)
        } else {
            None
        };

        let mut cnt = 0;
        let mut total_read = 0;

        while let Some(chunk) = resp.chunk().await? {
            let chunk_buffer = chunk.to_vec();
            let bcount = chunk_buffer.len();

            cnt += bcount;
            total_read += bcount;

            if !chunk_buffer.is_empty() {
                self.write_content(&chunk_buffer)?;
            }

            if let Some(ct_len) = ct_len {
                if total_read >= ct_len {
                    break;
                }
            } else if bcount == 0 {
                break;
            }
        }

        //如果配置较验验，则校验文件
        if !self.conf.file_md5.is_empty() && !self.compari_file_md5(&filepath, &self.conf.file_md5)
        {
            return Err(anyhow!("download ok but, file md5 not match"));
        }

        self.on_finish(filepath.clone());

        Ok(filepath)
    }

    //获取分片分段
    #[allow(dead_code)]
    fn get_chunk_offsets(&self) -> Vec<(u64, u64)> {
        let ct_len = self.conf.content_len;

        let chunk_size = self.conf.chunk_size;

        let num_chunks = ct_len / chunk_size;

        log::info!("num_chunks:{}", num_chunks);

        let mut sizes = Vec::new();

        for i in 0..num_chunks {
            let bound = if i == num_chunks - 1 {
                ct_len
            } else {
                ((i + 1) * chunk_size) - 1
            };

            sizes.push((i * chunk_size, bound));
        }

        if sizes.is_empty() {
            sizes.push((0, ct_len));
        }

        sizes
    }

    fn get_file_path(&self, filename: String) -> String {
        let mut path = PathBuf::from(filename.clone());
        if !self.conf.save_dir.is_empty() {
            path = PathBuf::from(&self.conf.save_dir);
            path.push(&filename);
        }
        path.to_str().unwrap_or_default().to_string()
    }

    //获取下灰复下载的位置
    fn get_resume_chunk_offsets(&self) -> Result<(Vec<(u64, u64)>, u64), Error> {
        if self.st_file.is_none() {
            return Err(anyhow!("st_file is none"));
        }

        let fname = format!("{}.st", self.filename);
        let mut path = PathBuf::from(fname.clone());

        if !self.conf.save_dir.is_empty() {
            path = PathBuf::from(&self.conf.save_dir);
            path.push(&fname);
        }

        let ct_len = self.conf.content_len;
        let chunk_size = self.conf.chunk_size;

        let input = fs::File::open(&path)?;
        let buf = BufReader::new(input);
        let mut already_downloaded_bytes = 0u64;

        let mut downloaded = vec![];
        for line in buf.lines() {
            let l = line?;
            let l = l.split(':').collect::<Vec<_>>();
            let n = (l[0].parse::<u64>()?, l[1].parse::<u64>()?);
            // 已下载字节数
            already_downloaded_bytes += n.0;
            //已下载位置
            downloaded.push(n);
        }
        downloaded.sort_by_key(|a| a.1);

        let mut chunks = vec![];

        let mut i: u64 = 0;
        for (bc, offset) in downloaded {
            if i == offset {
                i = offset + bc;
            } else {
                chunks.push((i, offset - 1));
                i = offset + bc;
            }
        }

        while (ct_len - i) > chunk_size {
            chunks.push((i, i + chunk_size - 1));
            i += chunk_size;
        }
        chunks.push((i, ct_len));

        Ok((chunks, already_downloaded_bytes))
    }

    //分片下载
    pub async fn chunk_download(&mut self) -> Result<String, Error> {
        let filepath = self.get_file_path(self.filename.clone());
        if self.compari_file_md5(&filepath, &self.conf.file_md5) {
            self.on_finish(filepath.clone());
            return Ok(filepath);
        }

        if !self.support_chunk {
            return Err(anyhow!("chunk download not support"));
        }

        //创建分片状态文件
        let filename = format!("{}.st", self.filename);
        self.st_file = Some(create_filehandler(&filename, &self.conf.save_dir, true)?);
        //分段内容
        let chunk_offsets_info = self.get_resume_chunk_offsets()?;

        //剩余分片
        let chunk_offsets = chunk_offsets_info.0;
        //文件已下载的进度
        let already_download = chunk_offsets_info.1;

        log::info!("already_download len :{}", already_download);
        log::info!("chunk_offsets count :{}", chunk_offsets.len());

        let mut append = false;

        //灰复下载，设置进度条进度
        if already_download > 0 {
            self.conf.download_len = already_download;
            self.on_progress();
            append = true;
        }

        self.file = Some(create_filehandler(
            &self.filename,
            &self.conf.save_dir,
            append,
        )?);

        let mut headers = self.conf.headers.clone();
        let mut num_workers = self.conf.num_workers;
        let max_retries = self.conf.max_retries;
        if num_workers == 0 {
            num_workers = 1;
        }

        if headers.contains_key(header::RANGE) {
            headers.remove(header::RANGE);
        }

        let Some(url) = self.conf.url.as_ref() else {
            return Err(anyhow!("url is empty"));
        };
        let mut req = self.httpclient.get(url.clone());

        if self.conf.timeout > 0 {
            req = req.timeout(Duration::from_secs(self.conf.timeout));
        }
        let req = req.headers(headers).build()?;

        let (data_tx, mut data_rx) = mpsc::channel::<(u64, u64, Vec<u8>)>(32);
        let (errors_tx, mut errors_rx) = mpsc::channel::<(u64, u64)>(32);

        let mut join_set = JoinSet::new(num_workers);

        for offsets in chunk_offsets {
            let p_data_tx = data_tx.clone();
            let p_errors_tx = errors_tx.clone();
            let Some(p_req) = req.try_clone() else {
                return Err(anyhow!("req.try_clone() err"));
            };

            join_set.spawn(async move {
                download_chunk(p_req, offsets, p_data_tx.clone(), p_errors_tx).await;
            });
        }

        let mut count = already_download;
        loop {
            if count == self.conf.content_len {
                break;
            }

            if let Some((byte_count, offset, buf)) = data_rx.recv().await {
                count += byte_count;

                self.chunk_write_content((byte_count, offset, &buf))?;

                match timeout(Duration::from_micros(1), errors_rx.recv()).await {
                    Ok(Some(offsets)) => {
                        if self.retries > max_retries {
                            if let Some(ref mut file) = self.file {
                                let _ = file.flush();
                            }
                            if let Some(ref mut file) = self.st_file {
                                let _ = file.flush();
                            }
                            return Err(anyhow!("max retries"));
                        }

                        self.retries += 1;
                        let data_tx = data_tx.clone();
                        let errors_tx = errors_tx.clone();
                        let Some(req) = req.try_clone() else {
                            return Err(anyhow!("req.try_clone() err"));
                        };

                        join_set.spawn(async move {
                            download_chunk(req, offsets, data_tx.clone(), errors_tx).await;
                        });
                    }
                    _ => {}
                }
            }
        }

        join_set.join_next().await;

        //如果配置较验验，则校验文件
        if !self.conf.file_md5.is_empty() && !self.compari_file_md5(&filepath, &self.conf.file_md5)
        {
            return Err(anyhow!("download ok but, file md5 not match"));
        }

        self.on_finish(filepath.clone());

        log::debug!("[downloader] download finish....");
        Ok(filepath)
    }
}

//分片下载
async fn download_chunk(
    req: Request,
    offsets: (u64, u64),
    sender: mpsc::Sender<(u64, u64, Vec<u8>)>,
    errors: mpsc::Sender<(u64, u64)>,
) {
    async fn inner(
        mut req: Request,
        offsets: (u64, u64),
        sender: mpsc::Sender<(u64, u64, Vec<u8>)>,
        start_offset: &mut u64,
    ) -> Result<(), Error> {
        log::trace!("download chunk:{}-{}", offsets.0, offsets.1);
        //0-10485759
        let byte_range = format!("bytes={}-{}", offsets.0, offsets.1);
        let headers = req.headers_mut();
        headers.insert(header::RANGE, HeaderValue::from_str(&byte_range)?);
        headers.insert(header::ACCEPT, HeaderValue::from_str("*/*")?);
        headers.insert(header::CONNECTION, HeaderValue::from_str("keep-alive")?);
        let mut resp = Client::new().execute(req).await?;

        let chunk_sz = offsets.1 - offsets.0;
        let mut cnt = 0u64;

        while let Some(chunk) = resp.chunk().await? {
            let byte_count = chunk.len() as u64;

            cnt += byte_count;

            sender
                .send((byte_count, *start_offset, chunk.to_vec()))
                .await?;

            *start_offset += byte_count;

            if cnt >= chunk_sz + 1 {
                break;
            }
        }
        log::trace!("[downloader] download chunk:ok...");
        Ok(())
    }

    let mut start_offset = offsets.0;
    let end_offset = offsets.1;

    if inner(req, offsets, sender, &mut start_offset)
        .await
        .is_err()
    {
        let _ = errors.send((start_offset, end_offset));
    }
}

//下载文件
fn get_file_handle(fname: &str, append: bool) -> io::Result<File> {
    if Path::new(fname).exists() {
        if append {
            OpenOptions::new().append(true).open(fname)
        } else {
            OpenOptions::new().write(true).open(fname)
        }
    } else {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(fname)
    }
}

//创建文件句柄
fn create_filehandler(
    filename: &str,
    save_dir: &str,
    append: bool,
) -> Result<BufWriter<File>, Error> {
    let mut fpath = filename.to_owned();
    // 创建保存文件目录
    if !save_dir.is_empty() {
        let path = Path::new(save_dir);
        if !path.exists() {
            fs::create_dir(save_dir)?;
        }
        let mut path = PathBuf::from(save_dir);
        path.push(filename);

        fpath = path
            .to_str()
            .map(|p| p.to_string())
            .unwrap_or("".to_string());
    }

    let handler = get_file_handle(fpath.as_str(), append)?;
    Ok(BufWriter::new(handler))
}

#[allow(dead_code)]
fn get_file_extension(file_path: &str) -> Option<&str> {
    let path = Path::new(file_path);
    path.extension().and_then(|s| s.to_str())
}
//生成文件名
//val:inline; filename="4f319a7a8cd6d322c6d938f7b8c2adb9.zip"; filename*=utf-8''4f319a7a8cd6d322c6d938f7b8c2adb9.zip
fn gen_filename(url: &Url, headers: Option<&HeaderMap>) -> String {
    let content = headers
        .and_then(|hdrs| hdrs.get(header::CONTENT_DISPOSITION))
        .and_then(|val| {
            let val = val.to_str().unwrap_or("");
            if val.contains("filename=") {
                Some(val)
            } else {
                None
            }
        })
        .and_then(|s| {
            let parts: Vec<&str> = s.rsplit(';').collect();
            let mut filename: Option<String> = None;
            for part in parts {
                if part.trim().starts_with("filename=") {
                    let name = part.trim().split('=').nth(1).unwrap_or("");
                    if !name.is_empty() {
                        let name = name.trim_start_matches('"').trim_end_matches('"');
                        filename = Some(name.to_owned());
                    }
                    break;
                }
            }
            filename
        });

    let filename = match content {
        Some(val) => val,
        None => {
            let name = &url.path().split('/').last().unwrap_or("");
            if !name.is_empty() {
                match decode_percent_encoded_data(name) {
                    Ok(val) => val,
                    _ => name.to_string(),
                }
            } else {
                "index.html".to_owned()
            }
        }
    };
    filename.trim().to_owned()
}

//url  decode
fn decode_percent_encoded_data(data: &str) -> Result<String, Error> {
    let mut unescaped_bytes: Vec<u8> = Vec::new();
    let mut bytes = data.bytes();

    while let Some(b) = bytes.next() {
        match b as char {
            '%' => {
                let bytes_to_decode = &[bytes.next().unwrap_or(0), bytes.next().unwrap_or(0)];
                let hex_str = std::str::from_utf8(bytes_to_decode)?;
                unescaped_bytes.push(u8::from_str_radix(hex_str, 16)?);
            }
            _ => {
                unescaped_bytes.push(b);
            }
        }
    }

    String::from_utf8(unescaped_bytes).map_err(|e| anyhow!(format!("String::from_utf8 ERR:{}", e)))
}
