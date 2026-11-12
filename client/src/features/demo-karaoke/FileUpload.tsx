import { useState, useRef } from 'react'
import { uploadMusicFile, type UploadResponse } from '../services/musicApi'

interface FileUploadProps {
  onUploadSuccess: (result: UploadResponse) => void
  onUploadError?: (error: string) => void
}

/**
 * 음악 파일 업로드 컴포넌트
 */
export function FileUpload({ onUploadSuccess, onUploadError }: FileUploadProps) {
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const [isUploading, setIsUploading] = useState(false)
  const [uploadStatus, setUploadStatus] = useState<string>('')
  const fileInputRef = useRef<HTMLInputElement>(null)

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) {
      setSelectedFile(file)
      setUploadStatus('')
      console.log('[FileUpload] File selected:', file.name)
    }
  }

  const handleUpload = async () => {
    if (!selectedFile) {
      setUploadStatus('파일을 선택해주세요.')
      return
    }

    setIsUploading(true)
    setUploadStatus('파일 업로드 중...')

    try {
      const result = await uploadMusicFile(selectedFile)

      if (result.success) {
        setUploadStatus(result.message || '업로드 성공!')
        onUploadSuccess(result)

        // 입력 초기화
        setSelectedFile(null)
        if (fileInputRef.current) {
          fileInputRef.current.value = ''
        }
      } else {
        const errorMsg = result.error || '업로드 실패'
        setUploadStatus(`오류: ${errorMsg}`)
        if (onUploadError) {
          onUploadError(errorMsg)
        }
      }
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : '알 수 없는 오류'
      setUploadStatus(`오류: ${errorMsg}`)
      if (onUploadError) {
        onUploadError(errorMsg)
      }
    } finally {
      setIsUploading(false)
    }
  }

  const handleClear = () => {
    setSelectedFile(null)
    setUploadStatus('')
    if (fileInputRef.current) {
      fileInputRef.current.value = ''
    }
  }

  return (
    <div className="file-upload-component">
      <div className="upload-area">
        <input
          ref={fileInputRef}
          type="file"
          id="file-upload-input"
          accept=".xml,.musicxml,.midi,.mid,.mp3,.pdf"
          className="file-input"
          onChange={handleFileChange}
          disabled={isUploading}
        />
        <label htmlFor="file-upload-input" className="file-label">
          <div className="upload-icon">📁</div>
          {selectedFile ? (
            <div className="selected-file-info">
              <p className="file-name">{selectedFile.name}</p>
              <p className="file-size">
                {(selectedFile.size / 1024 / 1024).toFixed(2)} MB
              </p>
            </div>
          ) : (
            <>
              <p>클릭하여 파일 선택</p>
              <span className="file-types">MusicXML, MIDI, MP3, PDF</span>
            </>
          )}
        </label>
      </div>

      {uploadStatus && (
        <div
          className={`status-message ${
            uploadStatus.includes('오류')
              ? 'error'
              : isUploading
              ? 'processing'
              : 'success'
          }`}
        >
          {isUploading && <div className="spinner"></div>}
          <p>{uploadStatus}</p>
        </div>
      )}

      <div className="upload-actions">
        <button
          className="upload-button"
          onClick={handleUpload}
          disabled={!selectedFile || isUploading}
        >
          {isUploading ? '업로드 중...' : '업로드'}
        </button>

        {selectedFile && !isUploading && (
          <button className="clear-button" onClick={handleClear}>
            취소
          </button>
        )}
      </div>
    </div>
  )
}
