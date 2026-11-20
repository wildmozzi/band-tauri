//! 파일 바꿔 넣기(계층 0): 설정(`settings`)·진단 zip(`diag::export`)이 함께 쓴다(#212 리뷰 L3 — 전에는 두 곳에 거의 같은 코드).

use std::io::Write;
use std::path::{Path, PathBuf};

/// `path`를 `bytes`로 바꿔 넣는다: 같은 폴더의 `<이름><suffix>`에 쓰고 sync한 뒤 rename한다(Windows는 기존 파일을 바꿔 넣는다).
/// 쓰는 도중 앱이 죽거나 디스크가 차도 `path`에는 옛 내용이 그대로 있다. 실패하면 임시 파일을 지운다.
pub(crate) fn write_replace(path: &Path, bytes: &[u8], suffix: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(suffix);
    let tmp = PathBuf::from(tmp);
    let res = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}
