"""
DEMUCS를 서버에 직접 통합하는 예시
Node.js 서버에서 Python 스크립트를 호출하는 방식

사용 방법:
1. pip install demucs
2. Node.js에서 child_process로 이 스크립트 실행
"""

import sys
import os
import demucs.separate
import torch

def separate_audio(input_path, output_dir, model='htdemucs'):
    """
    음원 분리 함수

    Args:
        input_path: 입력 MP3 파일 경로
        output_dir: 출력 디렉토리
        model: 사용할 모델 (htdemucs, htdemucs_ft, htdemucs_6s)

    Returns:
        출력 파일들의 경로 딕셔너리
    """

    # CUDA 사용 가능 여부 확인
    device = 'cuda' if torch.cuda.is_available() else 'cpu'
    print(f"Using device: {device}")

    # demucs 실행
    # --two-stems=vocals: vocals와 accompaniment만 분리
    # 4-stem: vocals, drums, bass, other
    cmd = [
        'python', '-m', 'demucs.separate',
        '-n', model,
        '-o', output_dir,
        '--device', device,
        input_path
    ]

    # 실제로는 demucs.separate 모듈을 직접 호출
    # 여기서는 간단히 subprocess 사용
    import subprocess
    result = subprocess.run(cmd, capture_output=True, text=True)

    if result.returncode != 0:
        print(f"Error: {result.stderr}", file=sys.stderr)
        return None

    # 출력 파일 경로 찾기
    song_name = os.path.splitext(os.path.basename(input_path))[0]
    output_path = os.path.join(output_dir, model, song_name)

    stems = {
        'vocals': os.path.join(output_path, 'vocals.wav'),
        'drums': os.path.join(output_path, 'drums.wav'),
        'bass': os.path.join(output_path, 'bass.wav'),
        'other': os.path.join(output_path, 'other.wav')
    }

    # 파일 존재 확인
    for stem, path in list(stems.items()):
        if not os.path.exists(path):
            del stems[stem]

    return stems

if __name__ == '__main__':
    if len(sys.argv) < 3:
        print("Usage: python demucs_separate.py <input_file> <output_dir> [model]")
        sys.exit(1)

    input_file = sys.argv[1]
    output_dir = sys.argv[2]
    model = sys.argv[3] if len(sys.argv) > 3 else 'htdemucs'

    print(f"Separating: {input_file}")
    print(f"Output: {output_dir}")
    print(f"Model: {model}")

    stems = separate_audio(input_file, output_dir, model)

    if stems:
        print("Separation complete!")
        import json
        print(json.dumps(stems))
    else:
        print("Separation failed!", file=sys.stderr)
        sys.exit(1)
