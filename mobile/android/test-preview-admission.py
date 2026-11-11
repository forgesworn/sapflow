#!/usr/bin/env python3
"""Host-only policy and structural regression checks; no APK or device."""
import pathlib,subprocess,tempfile
ROOT=pathlib.Path(__file__).resolve().parents[2]
source='mobile/android/app/src/main/java/dev/forgesworn/sapflow/broadcaster/'
renderer=ROOT/source/'CameraGlRenderer.kt';current=renderer.read_text()
base=subprocess.check_output(['git','show','75bf50a43756b2638a5acf8c13b3f2c8761de230:'+str(renderer.relative_to(ROOT))],cwd=ROOT,text=True)
def section(text,start,end):return text.split(start,1)[1].split(end,1)[0]
# Privacy concealment and the independent replacement-slate path must stay
# byte-identical to the pinned base, with no live-preview admission dependency.
assert section(current,'    fun concealPreview()', '    private fun drawSlate(')==section(base,'    fun concealPreview()', '    private fun drawSlate(')
assert current.count('previewAdmission.claim(')==1
branch=section(current,'    private fun renderLatest()', '    /** Preview is a borrowed window')
old_branch=section(base,'    private fun renderLatest()', '    /** Preview is a borrowed window')
assert branch.replace('if (BuildConfig.DIAGNOSTIC_PREVIEW_FPS == 0 || previewAdmission.claim(System.nanoTime())) renderPreview(false)', 'renderPreview(false)') == old_branch
assert branch.index('eglSwapBuffers(display, encoder)') < branch.index('confirmSource(') < branch.index('previewAdmission.claim(')
assert 'if (previewAllowed() && preview != EGL14.EGL_NO_SURFACE && live.get() && requestedSource.get() == sourceToken)' in branch
assert 'previewAdmission.reset()' in section(current,'    fun replaceSource(', '    private fun createSource(')
assert 'previewAdmission.reset()' in section(current,'    private fun detachPreview()', '    private fun guarded(')
build=(ROOT/'mobile/android/app/build.gradle.kts').read_text()
assert 'buildConfigField("int", "DIAGNOSTIC_PREVIEW_FPS", "0")' in build
debug=section(build,'        getByName("debug") {', '        create("qualification") {')
assert 'sapflowDebugPreviewFps' in debug and 'setOf("0", "15")' in debug
qualification=section(build,'        create("qualification") {', '    sourceSets {')
assert 'DIAGNOSTIC_PREVIEW_FPS' not in qualification
with tempfile.TemporaryDirectory(prefix='sapflow-preview-host-') as tmp:
 jar=pathlib.Path(tmp)/'test.jar'
 subprocess.run(['kotlinc',str(ROOT/source/'PreviewAdmission.kt'),str(ROOT/'mobile/android/host-tests/PreviewAdmissionHostTest.kt'),'-include-runtime','-d',str(jar)],check=True,timeout=90)
 subprocess.run(['java','-jar',str(jar)],check=True,timeout=15)
print('PASS: encoder precedes preview cap; privacy paths unchanged; reset and debug-only selection present')
