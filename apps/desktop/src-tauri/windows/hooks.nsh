; NSIS 卸载钩子（设计 §5.4、§12；实施计划阶段 1 任务 1.8）。
;
; 由 Tauri 2 的 NSIS 模板通过 installerHooks 在生成脚本里 `!include`，位置在顶层
; （早于所有 Section），因此本文件可以在顶层声明 Var。
;
; ── 已对照 Tauri 2 tauri-bundler dev 分支源码逐条核实（2026-09-23）──
;
; 1) PREUNINSTALL / POSTUNINSTALL 时机（`Section Uninstall` 逐字节核对）：
;    !insertmacro NSIS_HOOK_PREUNINSTALL          ← 早于一切
;    !insertmacro CheckIfAppIsRunning ...          ← 可能 Abort 整个卸载
;    Delete 主程序 / 资源 / externalBin（含 codex-helper-credential.exe）/ 卸载器
;    RMDir "$INSTDIR"
;    ${If} $UpdateMode <> 1  移除快捷方式 / 注册表 / 自启项 / 应用数据
;    !insertmacro NSIS_HOOK_POSTUNINSTALL          ← 文件已删除之后
;    因此：a) 若把破坏性操作放在 PREUNINSTALL，CheckIfAppIsRunning 检测到主程序在
;    运行、用户点“取消”时会 Abort，但清理已经执行，导致“程序还在、配置已被清空”；
;    b) 到 POSTUNINSTALL 时 codex-helper-credential.exe 已随 externalBin 一起被
;    Delete，不能直接从 $INSTDIR 执行。
;    本实现的取舍：在 PREUNINSTALL 里只询问用户是否清除 Key，并把凭据程序复制到
;    $PLUGINSDIR（卸载器进程退出时自动清理的临时目录）；真正的 cleanup 调用移到
;    POSTUNINSTALL，从 $PLUGINSDIR 执行，这样既晚于“主程序是否在运行”的判断（用户
;    选择取消卸载则整个 Section 连同本钩子一起被 Abort，不会执行 cleanup），也晚于
;    文件删除（凭据程序已提前复制出来，不受影响）。
;
; 2) 更新 / 覆盖安装触发的卸载必须跳过（不弹窗、不 cleanup）。模板里能识别到的更新
;    场景有两种，缺一都会误判：
;    a. `$UpdateMode = 1`：卸载器自身在 un.onInit 里用
;       `${GetOptions} $CMDLINE "/UPDATE" $UpdateMode` 解析出来，只有当“调用它的那个
;       安装会话自己也是带着 /UPDATE 启动”时才会转发 /UPDATE（installer.nsi 的
;       PageLeaveReinstall: `${IfThen} $UpdateMode = 1 ${|} StrCpy $R1 "$R1 /UPDATE" ${|}`），
;       这条路径对应 tauri-plugin-updater 之类的静默自更新，本项目未启用（设计 §1.2）。
;    b. 手动重新运行新版本安装包、在“检测到已安装”页选择默认的“先卸载再安装”：这是
;       本项目实际会遇到的路径。此时新安装会话自身没有 /UPDATE（用户是手动双击运行
;       的），PageLeaveReinstall 里 `${IfThen} $UpdateMode = 1 ...` 不成立，所以调用旧
;       版本卸载器时**不会**附加 /UPDATE；旧卸载器的 $UpdateMode 因而仍是空/0——如果
;       只判断 `$UpdateMode <> 1`，这种日常升级会被当成真实卸载，弹窗且执行 cleanup，
;       等价于静默关闭受管模式（cleanup 按 §5.2 第 4 步把 enabled 置为 false，之后
;       §5.3 的启动对账在 enabled=false 时只检查残留、不会重建配置）。
;       区分手段：PageLeaveReinstall 调用旧卸载器时**无条件**附加了 `_?=$4`
;       （$4 即安装目录）：
;         `StrCpy $R1 "$R1 _?=$4" ; append uninstall directory`
;       `_?=` 是 NSIS 卸载器的内置约定参数：带着它启动时，卸载器不会把自己复制到
;       临时目录再重新以 `_?=$INSTDIR\` 自我重启，而是直接在传入的目录原地运行；
;       不带它启动（用户从控制面板 / 双击 $INSTDIR\uninstall.exe 触发的“真实卸载”）
;       时，卸载器才会先自我复制到 `%TEMP%\~nsuXXXX.tmp\` 再重新执行。也就是说：
;         - 覆盖安装触发：卸载器原地运行于 $INSTDIR，此时 $EXEDIR == $INSTDIR。
;         - 用户手动触发真实卸载：卸载器最终运行的是临时目录里的自身拷贝，
;           $EXEDIR 是 %TEMP%\~nsuXXXX.tmp，不等于 $INSTDIR。
;       因此用 `$UpdateMode <> 1 AND $EXEDIR != $INSTDIR` 联合判断，能同时覆盖两种
;       更新场景。该结论依赖 NSIS 卸载器自拷贝这一内置行为，未在 Windows 实机复现，
;       上线前需按设计 §13 补一次真机验证：手动升级不弹窗/不清理，真实卸载弹窗/清理。
;
; 3) `%LOCALAPPDATA%\Programs\CodexHelper` 与实际默认安装目录不符 —— 与本文件无关的
;    独立结论，记录于此便于统一查阅：tauri-bundler installer.nsi 的 .onInit 中，
;    `installMode == "currentUser"` 分支是
;       `StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"`
;    即实际默认安装目录是 `%LOCALAPPDATA%\CodexHelper`，与设计 §12 写的
;    `%LOCALAPPDATA%\Programs\CodexHelper` 不同；且与 helper-core
;    `paths::default_data_dir()`（`data_local_dir().join("CodexHelper")`，同为
;    `%LOCALAPPDATA%\CodexHelper`）完全重合——exe、state.json、日志会同目录存放。
;    已知后果：`RMDir "$INSTDIR"`（非递归）卸载时不会误删这些文件，但也不会清掉，
;    会遗留在 `%LOCALAPPDATA%\CodexHelper` 下；勾选“删除应用数据”清的是
;    `$LOCALAPPDATA\com.codexhelper.desktop`（BUNDLEID），不是这个目录；
;    EstimatedSize 会把日志体积计入安装包体积估算。可选方案：① 接受现状、在设计里把
;    §12 的目录说明改成 `%LOCALAPPDATA%\CodexHelper`；② 通过
;    `bundle.windows.nsis.template` 自定义模板改写默认目录为
;    `%LOCALAPPDATA%\Programs\CodexHelper`（本任务不允许改模板，需上报决策）；
;    ③ 调整 `DATA_DIR_NAME` 让数据目录改名避让（属契约变更，需走 contract_requests）。
;    本任务未改动任何代码来回应这一条，仅如实记录，交由主会话裁决。

Var CodexHelperRealUninstall
Var CodexHelperPurgeKey

!macro NSIS_HOOK_PREUNINSTALL
  StrCpy $CodexHelperRealUninstall 0
  StrCpy $CodexHelperPurgeKey 0

  ${If} $UpdateMode <> 1
  ${AndIf} $EXEDIR != $INSTDIR
    StrCpy $CodexHelperRealUninstall 1

    ; 在文件被删除之前，把凭据程序复制到卸载器专属的临时目录，供
    ; POSTUNINSTALL 阶段执行；$PLUGINSDIR 随卸载器进程退出自动清理。
    InitPluginsDir
    CopyFiles /SILENT "$INSTDIR\codex-helper-credential.exe" "$PLUGINSDIR"

    ; 静默卸载（/S）不弹窗，直接按 IDNO 处理，即保留 Key——清除凭据这种破坏性
    ; 操作不应在无人值守时默认发生；对话框默认聚焦“否”（MB_DEFBUTTON2），
    ; 避免误回车删除 Key。
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
      "是否同时清除已保存的 API Key？$\r$\n$\r$\n选择“否”仅撤销 Codex 的受管配置，Credential Manager 中的 Key 予以保留。" \
      /SD IDNO IDYES codexhelper_preuninstall_purge
    Goto codexhelper_preuninstall_asked

    codexhelper_preuninstall_purge:
      StrCpy $CodexHelperPurgeKey 1

    codexhelper_preuninstall_asked:
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; 走到这里说明 CheckIfAppIsRunning 没有 Abort（主程序未运行，或用户已同意关闭），
  ; 且应用文件已删除；此时执行 cleanup 才不会出现“配置已清、程序还在”的中间态。
  ${If} $CodexHelperRealUninstall = 1
    ${If} $CodexHelperPurgeKey = 1
      nsExec::ExecToLog /TIMEOUT=30000 '"$PLUGINSDIR\codex-helper-credential.exe" cleanup --purge-key'
    ${Else}
      nsExec::ExecToLog /TIMEOUT=30000 '"$PLUGINSDIR\codex-helper-credential.exe" cleanup'
    ${EndIf}
    Pop $0
    ; nsExec::ExecToLog 启动失败时压栈的是字符串 "error"（超时是 "timeout"），
    ; 必须按字符串比较，否则非数字字符串会被当整数 0 处理而漏记日志。
    ; cleanup 失败（含无法启动）不阻断卸载——程序已经删完，用户没有回退的意义——
    ; 但把结果写进安装日志，便于事后排查；$0 只是退出码/状态字符串，不携带 Key。
    ${If} $0 != "0"
      DetailPrint "codex-helper-credential cleanup 退出码/状态 $0（不阻断卸载）"
    ${EndIf}
  ${EndIf}
!macroend
