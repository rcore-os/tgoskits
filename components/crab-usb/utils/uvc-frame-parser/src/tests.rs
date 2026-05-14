#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Parser, Pixel};
    use crab_uvc::{UncompressedFormat, VideoFormat};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_and_read_format_info_toml() {
        // 创建临时目录
        let temp_dir = TempDir::new().unwrap();
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");

        // 创建解析器
        let parser = Parser::new(input_dir, output_dir).await;

        // 创建测试的视频格式
        let video_format = VideoFormat::Uncompressed {
            width: 1280,
            height: 720,
            frame_rate: 60,
            format_type: UncompressedFormat::Yuy2,
        };

        // 写入格式信息
        parser.write_format_info(&video_format).await.unwrap();

        // 读取格式信息
        let video_info = parser.read_format_info().await.unwrap();

        // 验证数据
        assert_eq!(video_info.width, 1280);
        assert_eq!(video_info.height, 720);
        assert_eq!(video_info.fps, 60);
        assert!(matches!(video_info.pixel, Pixel::Yuy2));
    }

    #[tokio::test]
    async fn test_different_pixel_formats() {
        let temp_dir = TempDir::new().unwrap();
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        let parser = Parser::new(input_dir, output_dir).await;

        // 测试 MJPEG 格式
        let mjpeg_format = VideoFormat::Mjpeg {
            width: 1920,
            height: 1080,
            frame_rate: 30,
        };

        parser.write_format_info(&mjpeg_format).await.unwrap();
        let video_info = parser.read_format_info().await.unwrap();

        assert_eq!(video_info.width, 1920);
        assert_eq!(video_info.height, 1080);
        assert_eq!(video_info.fps, 30);
        assert!(matches!(video_info.pixel, Pixel::Mjpeg));
    }
}
