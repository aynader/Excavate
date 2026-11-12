@echo off
rem Builds Excavate's listener and the whisper.cpp it runs on, into a package folder:
rem
rem   build.bat vulkan <whisper.cpp source> <out>   excavate-listen.exe, whisper.cpp's libraries, every processor variant
rem                                                 and Vulkan (needs glslc, the Vulkan and SPIR-V headers: VULKAN_SDK)
rem   build.bat cuda <whisper.cpp source> <out>     the CUDA add-on: ggml-cuda.dll and NVIDIA's runtime and cuBLAS
rem                                                 (needs CUDA_PATH: the CUDA toolkit)
rem
rem Needs Visual Studio 2022's C++ tools, CMake and Ninja (Visual Studio's own). The build folder is kept short:
rem Vulkan's shader generator nests deep and Windows' paths stop at 260 characters. Don't edit this while it runs.
setlocal
set "KIND=%~1"
set "SRC=%~2"
set "OUT=%~3"
if "%OUT%"=="" goto usage
set "HERE=%~dp0"
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\Installer;%PATH%"
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
set "PATH=C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%"
set "BUILD=%TEMP%\xl-%KIND%"
set "COMMON=-G Ninja -DCMAKE_BUILD_TYPE=Release -DWHISPER_SOURCE=%SRC% -DWHISPER_BUILD_TESTS=OFF -DWHISPER_SDL2=OFF -DBUILD_SHARED_LIBS=ON -DGGML_BACKEND_DL=ON -DGGML_NATIVE=OFF"
if "%KIND%"=="vulkan" goto vulkan
if "%KIND%"=="cuda" goto cuda
goto usage

:vulkan
if "%VULKAN_SDK%"=="" echo VULKAN_SDK isn't set & exit /b 1
set "PATH=%VULKAN_SDK%\bin;%PATH%"
if exist "%BUILD%" rmdir /s /q "%BUILD%"
cmake -S "%HERE%." -B "%BUILD%" %COMMON% -DGGML_CPU_ALL_VARIANTS=ON -DGGML_VULKAN=ON "-DCMAKE_PREFIX_PATH=%VULKAN_SDK%" || exit /b 1
cmake --build "%BUILD%" -j 16 || exit /b 1
if not exist "%OUT%" mkdir "%OUT%"
copy /y "%BUILD%\bin\excavate-listen.exe" "%OUT%\" >nul || exit /b 1
copy /y "%BUILD%\bin\*.dll" "%OUT%\" >nul || exit /b 1
rem The build's number (CMakeLists.txt): an unpacked listener of an earlier build is brought up to date.
copy /y "%BUILD%\bin\excavate-listen-build.txt" "%OUT%\" >nul || exit /b 1
rem Microsoft's C++ runtime beside it (its app-local redistribution), so nothing has to be installed on the PC.
for /d %%d in ("%VCToolsRedistDir%x64\Microsoft.VC*.CRT") do copy /y "%%d\msvcp140.dll" "%OUT%\" >nul & copy /y "%%d\vcruntime140.dll" "%OUT%\" >nul & copy /y "%%d\vcruntime140_1.dll" "%OUT%\" >nul
if not exist "%OUT%\vcruntime140.dll" echo Visual C++'s runtime wasn't found & exit /b 1
rem The notices that go with the copies: whisper.cpp's (MIT) and Excavate's own.
copy /y "%SRC%\LICENSE" "%OUT%\LICENSE-whisper.cpp.txt" >nul || exit /b 1
copy /y "%HERE%..\..\LICENSE" "%OUT%\LICENSE-excavate-listen.txt" >nul || exit /b 1
rem (whisper.cpp's own programs, for measuring; not shipped.)
if not exist "%OUT%\tools" mkdir "%OUT%\tools"
copy /y "%BUILD%\bin\whisper-cli.exe" "%OUT%\tools\" >nul
copy /y "%BUILD%\bin\parakeet-cli.exe" "%OUT%\tools\" >nul
goto done

:cuda
if "%CUDA_PATH%"=="" echo CUDA_PATH isn't set & exit /b 1
set "PATH=%CUDA_PATH%\bin;%PATH%"
if exist "%BUILD%" rmdir /s /q "%BUILD%"
rem Turing to Blackwell (GTX 16 and RTX 20 onwards), and PTX for what comes next.
cmake -S "%HERE%." -B "%BUILD%" %COMMON% -DGGML_CPU=OFF -DGGML_CUDA=ON "-DCMAKE_CUDA_ARCHITECTURES=75-real;80-real;86-real;89-real;120-real;120-virtual" || exit /b 1
cmake --build "%BUILD%" -j 16 --target ggml-cuda || exit /b 1
if not exist "%OUT%" mkdir "%OUT%"
copy /y "%BUILD%\bin\ggml-cuda.dll" "%OUT%\" >nul || exit /b 1
for %%f in (cudart64_12.dll cublas64_12.dll cublasLt64_12.dll) do copy /y "%CUDA_PATH%\bin\%%f" "%OUT%\" >nul || exit /b 1
rem NVIDIA's license for its redistributable runtime and cuBLAS, and whisper.cpp's for its CUDA library.
copy /y "%CUDA_PATH%\EULA.txt" "%OUT%\EULA-NVIDIA-CUDA.txt" >nul || exit /b 1
copy /y "%SRC%\LICENSE" "%OUT%\LICENSE-whisper.cpp.txt" >nul || exit /b 1
goto done

:usage
echo usage: build.bat vulkan^|cuda ^<whisper.cpp source^> ^<out^>
exit /b 2

:done
rmdir /s /q "%BUILD%"
echo built %KIND% into %OUT%
