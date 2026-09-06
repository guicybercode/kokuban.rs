package com.kokuban.terminal;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.graphics.Rect;
import android.os.Build;
import android.os.Bundle;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowManager;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.FrameLayout;
import android.widget.Toast;

/** Platform-only editor adapter. Rendering, terminal state and key encoding live in Rust. */
public final class KokubanActivity extends NativeActivity {
    static { System.loadLibrary("kokuban"); }

    private static final int MAX_EDITOR_UNITS = 16384;
    private EditorView editor;
    private boolean destroyed;
    private final Rect previousViewport = new Rect(-1, -1, -1, -1);
    private boolean previousKeyboard;

    private static native void nativeText(int operation, String text);
    private static native void nativeDelete(int before, int after);
    private static native void nativeKey(int code, int unicode, int modifiers);
    private static native void nativeViewport(int left, int top, int right, int bottom, boolean keyboard);
    private static native void nativeClipboard(String text);

    @Override protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        editor = new EditorView(this);
        // NativeActivity owns the window surface. This transparent, non-clickable view
        // supplies Android's mandatory editor object without replacing that surface.
        addContentView(editor, new FrameLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        editor.requestFocus();
        getWindow().getDecorView().getViewTreeObserver().addOnGlobalLayoutListener(this::reportViewport);
    }

    @Override protected void onDestroy() {
        destroyed = true;
        editor = null;
        super.onDestroy();
    }

    public void refreshInput() {
        runOnUiThread(() -> {
            if (destroyed || editor == null) return;
            previousViewport.set(-1, -1, -1, -1);
            reportViewport();
        });
    }

    public void setKeyboardVisible(boolean visible) {
        runOnUiThread(() -> {
            if (destroyed || editor == null) return;
            InputMethodManager manager = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            if (visible) {
                editor.requestFocus();
                manager.showSoftInput(editor, InputMethodManager.SHOW_IMPLICIT);
            } else {
                manager.hideSoftInputFromWindow(editor.getWindowToken(), 0);
            }
        });
    }

    public void copyText(String text) {
        runOnUiThread(() -> {
            if (destroyed) return;
            ClipboardManager manager = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
            manager.setPrimaryClip(ClipData.newPlainText("Kokuban", text));
        });
    }

    public void pasteText() {
        runOnUiThread(() -> {
            if (destroyed) return;
            ClipboardManager manager = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
            ClipData clip = manager.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) return;
            // Read plain text only. Do not resolve arbitrary clipboard URIs on the UI thread.
            CharSequence text = clip.getItemAt(0).getText();
            if (text != null && text.length() <= MAX_EDITOR_UNITS) nativeClipboard(text.toString());
        });
    }

    public void showError(String message) {
        runOnUiThread(() -> {
            if (!destroyed) Toast.makeText(this, "Kokuban: " + message, Toast.LENGTH_LONG).show();
        });
    }

    public String nativeLibraryDir() { return getApplicationInfo().nativeLibraryDir; }

    private void reportViewport() {
        if (destroyed || editor == null) return;
        View content = findViewById(android.R.id.content);
        View decor = getWindow().getDecorView();
        if (content.getWidth() == 0 || content.getHeight() == 0) return;
        Rect visible = new Rect();
        decor.getWindowVisibleDisplayFrame(visible);
        int[] contentOrigin = new int[2];
        content.getLocationOnScreen(contentOrigin);
        if (!visible.intersect(contentOrigin[0], contentOrigin[1],
                contentOrigin[0] + content.getWidth(), contentOrigin[1] + content.getHeight())) return;
        int[] surfaceOrigin = new int[2];
        decor.getLocationOnScreen(surfaceOrigin);
        Rect viewport = new Rect(Math.max(0, visible.left - surfaceOrigin[0]),
            Math.max(0, visible.top - surfaceOrigin[1]),
            Math.max(0, visible.right - surfaceOrigin[0]),
            Math.max(0, visible.bottom - surfaceOrigin[1]));
        WindowInsets windowInsets = decor.getRootWindowInsets();
        boolean keyboard = Build.VERSION.SDK_INT >= 30 && windowInsets != null
            ? windowInsets.isVisible(WindowInsets.Type.ime())
            : decor.getHeight() - visible.height() > getResources().getDisplayMetrics().density * 150;
        if (!viewport.equals(previousViewport) || keyboard != previousKeyboard) {
            previousViewport.set(viewport);
            previousKeyboard = keyboard;
            nativeViewport(viewport.left, viewport.top, viewport.right, viewport.bottom, keyboard);
        }
    }

    private static final class EditorView extends View {
        EditorView(Context context) {
            super(context);
            setFocusable(true);
            setFocusableInTouchMode(true);
            setContentDescription("Entrada do terminal");
            setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_YES);
        }

        @Override public boolean onCheckIsTextEditor() { return true; }

        @Override public InputConnection onCreateInputConnection(EditorInfo info) {
            info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            info.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_FULLSCREEN
                | EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING | EditorInfo.IME_ACTION_NONE;
            info.initialSelStart = 0;
            info.initialSelEnd = 0;
            return new TerminalConnection(this);
        }

        @Override public void onInitializeAccessibilityNodeInfo(AccessibilityNodeInfo info) {
            super.onInitializeAccessibilityNodeInfo(info);
            info.setClassName("android.widget.EditText");
            info.setEditable(true);
            info.setMultiLine(true);
            info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_TEXT);
        }

        @Override public boolean performAccessibilityAction(int action, Bundle arguments) {
            if (action == AccessibilityNodeInfo.ACTION_SET_TEXT && arguments != null) {
                CharSequence value = arguments.getCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE);
                if (value != null && value.length() <= MAX_EDITOR_UNITS) {
                    nativeText(0, value.toString());
                    return true;
                }
            }
            return super.performAccessibilityAction(action, arguments);
        }
    }

    /** The editable contains only the active IME transaction, never terminal output. */
    private static final class TerminalConnection extends BaseInputConnection {
        private final View view;
        private int batchDepth;
        private boolean selectionDirty;
        private boolean closed;

        TerminalConnection(View view) {
            super(view, true);
            this.view = view;
            Selection.setSelection(getEditable(), 0);
        }

        private void selectionChanged() {
            selectionDirty = true;
            if (batchDepth != 0) return;
            Editable text = getEditable();
            InputMethodManager manager = (InputMethodManager) view.getContext().getSystemService(Context.INPUT_METHOD_SERVICE);
            manager.updateSelection(view, Selection.getSelectionStart(text), Selection.getSelectionEnd(text),
                getComposingSpanStart(text), getComposingSpanEnd(text));
            selectionDirty = false;
        }

        private void clearTransaction() {
            getEditable().clear();
            removeComposingSpans(getEditable());
            Selection.setSelection(getEditable(), 0);
            selectionChanged();
        }

        @Override public boolean beginBatchEdit() {
            if (closed) return false;
            batchDepth++;
            return true;
        }

        @Override public boolean endBatchEdit() {
            if (closed) return false;
            if (batchDepth > 0) batchDepth--;
            if (selectionDirty) selectionChanged();
            return batchDepth > 0;
        }

        @Override public boolean setComposingText(CharSequence text, int cursor) {
            if (closed || text == null || text.length() > MAX_EDITOR_UNITS) return false;
            boolean result = super.setComposingText(text, cursor);
            nativeText(1, getEditable().toString());
            selectionChanged();
            return result;
        }

        @Override public boolean commitText(CharSequence text, int cursor) {
            if (closed || text == null || text.length() > MAX_EDITOR_UNITS) return false;
            boolean result = super.commitText(text, cursor);
            nativeText(0, getEditable().toString());
            clearTransaction();
            return result;
        }

        @Override public boolean finishComposingText() {
            if (closed) return false;
            super.finishComposingText();
            nativeText(2, "");
            clearTransaction();
            return true;
        }

        @Override public boolean setSelection(int start, int end) {
            if (closed) return false;
            boolean result = super.setSelection(start, end);
            selectionChanged();
            return result;
        }

        @Override public boolean deleteSurroundingText(int before, int after) {
            return deleteAroundCursor(before, after, false);
        }

        @Override public boolean deleteSurroundingTextInCodePoints(int before, int after) {
            return deleteAroundCursor(before, after, true);
        }

        private boolean deleteAroundCursor(int before, int after, boolean codePoints) {
            if (closed || before < 0 || after < 0) return false;
            Editable text = getEditable();
            if (text.length() == 0) {
                nativeDelete(before, after);
                return true;
            }
            // Delegate composing spans, UTF-16 boundaries and selection to the OS editor.
            boolean result = codePoints ? super.deleteSurroundingTextInCodePoints(before, after)
                : super.deleteSurroundingText(before, after);
            nativeText(1, text.toString());
            selectionChanged();
            return result;
        }

        @Override public boolean sendKeyEvent(KeyEvent event) {
            if (closed) return false;
            if (event.getAction() == KeyEvent.ACTION_DOWN) {
                finishComposingText();
                nativeKey(event.getKeyCode(), event.getUnicodeChar(event.getMetaState()), event.getMetaState());
            }
            return true;
        }

        @Override public boolean performEditorAction(int action) {
            if (closed) return false;
            finishComposingText();
            nativeKey(KeyEvent.KEYCODE_ENTER, 0, 0);
            return true;
        }

        @Override public void closeConnection() {
            if (!closed) finishComposingText();
            closed = true;
            super.closeConnection();
        }
    }
}
