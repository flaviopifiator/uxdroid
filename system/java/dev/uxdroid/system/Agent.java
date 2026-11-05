package dev.uxdroid.system;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.ActivityThread;
import android.content.Context;
import android.content.pm.ParceledListSlice;
import android.graphics.Rect;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.view.WindowManagerGlobal;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityManager;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityNodeProvider;
import android.view.accessibility.IAccessibilityManager;
import android.view.accessibility.IAccessibilityManagerClient;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import com.android.internal.util.IntPair;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * What an agent reads of the app, on the main thread: each window's views and accessibility nodes as
 * one snapshot. A view with an AccessibilityNodeProvider, such as Flutter's FlutterView, reports the
 * provider's virtual nodes; any other view reports its own node and its children. Some providers
 * only answer while accessibility is on, so it is turned on, for this process only, while an agent
 * that asked for nodes is connected, and off when the last one leaves.
 */
public final class Agent {
    private Agent() {}

    /** Text of one field is cut to this many characters. */
    private static final int MAX_TEXT = 1000;
    /** After accessibility turns on, providers build their nodes over a few frames. */
    private static final long SETTLE_STEP_MS = 100;
    private static final long SETTLE_MIN_MS = 300;
    private static final long SETTLE_MAX_MS = 3000;

    private static long snapshots;
    private static Accessibility accessibility;
    /** Snapshots still waiting for nodes to settle after accessibility turned on. */
    private static int settling;

    /**
     * Answers a ui command with a snapshot, after the nodes settle if accessibility just turned on,
     * or is still settling for another agent's command. The limits are the protocol's, checked again
     * here so no caller of this class can ask for an unbounded walk.
     */
    static void ui(long sequence, JSONObject arguments) throws JSONException {
        int maxNodes = Math.max(1, Math.min(5000, arguments.optInt("max_nodes", 1000)));
        int maxDepth = Math.max(1, Math.min(128, arguments.optInt("max_depth", 64)));
        if (!enable(true) && settling == 0) {
            Control.answer(sequence, snapshot(maxNodes, maxDepth).toString(), null, null);
            return;
        }
        settling++;
        settle(sequence, maxNodes, maxDepth, SystemClock.uptimeMillis(), -1, 0);
    }

    private static void settle(long sequence, int maxNodes, int maxDepth, long since, int before, int same) {
        new Handler(Looper.getMainLooper()).postDelayed(() -> {
            try {
                JSONObject snapshot = snapshot(maxNodes, maxDepth);
                int count = snapshot.getInt("nodes");
                int stable = count == before ? same + 1 : 0;
                long waited = SystemClock.uptimeMillis() - since;
                if ((stable >= 2 && waited >= SETTLE_MIN_MS) || waited >= SETTLE_MAX_MS) {
                    settling--;
                    Control.answer(sequence, snapshot.toString(), null, null);
                } else {
                    settle(sequence, maxNodes, maxDepth, since, count, stable);
                }
            } catch (JSONException | RuntimeException e) {
                settling--;
                Control.answer(sequence, null, "failed", String.valueOf(e));
            }
        }, SETTLE_STEP_MS);
    }

    /** The last agent left: accessibility goes back off. */
    static JSONObject release() {
        enable(false);
        return new JSONObject();
    }

    /** Text the seat's keymap cannot type, committed into the focused view as an input method would. */
    static JSONObject commit(String text) throws JSONException, ActivityStack.Refusal {
        View focused = null;
        List<View> roots = WindowManagerGlobal.getInstance().getWindowViews();
        for (int i = roots.size() - 1; i >= 0 && focused == null; i--) {
            if (roots.get(i).hasWindowFocus()) focused = roots.get(i).findFocus();
        }
        if (focused == null) throw new ActivityStack.Refusal("failed", "no view has focus to type into");
        InputConnection connection = focused.onCreateInputConnection(new EditorInfo());
        if (connection == null) {
            throw new ActivityStack.Refusal("failed", focused.getClass().getName() + " takes no text");
        }
        connection.commitText(text, 1);
        return new JSONObject().put("committed", text.codePointCount(0, text.length()));
    }

    /** Turns accessibility on or off for this process; true when it just turned on. */
    private static boolean enable(boolean on) {
        if (accessibility == null) {
            if (!on) return false;
            accessibility = new Accessibility();
            Services.register(Context.ACCESSIBILITY_SERVICE, IAccessibilityManager.class, accessibility);
        }
        boolean changed = accessibility.set(on);
        // The manager connects to the service the first time it needs it, and registers its client.
        Context context = ActivityThread.currentApplication();
        if (on && context != null) {
            AccessibilityManager.getInstance(context)
                    .getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_ALL_MASK);
        }
        return on && changed;
    }

    /** IAccessibilityManager in this process: tells its clients the state agents asked for. */
    public static final class Accessibility {
        private final List<IAccessibilityManagerClient> clients = new ArrayList<>();
        private int state;

        synchronized boolean set(boolean on) {
            int wanted = on ? AccessibilityManager.STATE_FLAG_ACCESSIBILITY_ENABLED : 0;
            if (wanted == state) return false;
            state = wanted;
            for (IAccessibilityManagerClient client : clients) {
                try {
                    client.setState(state);
                } catch (android.os.RemoteException | RuntimeException e) {
                    System.err.println("uxdroid: an accessibility client did not take its state: " + e);
                }
            }
            return true;
        }

        public synchronized long addClient(IAccessibilityManagerClient client, int userId) {
            clients.add(client);
            return IntPair.of(state, AccessibilityEvent.TYPES_ALL_MASK);
        }

        public synchronized boolean removeClient(IAccessibilityManagerClient client, int userId) {
            return clients.remove(client);
        }

        public ParceledListSlice<AccessibilityServiceInfo> getInstalledAccessibilityServiceList(int userId) {
            return ParceledListSlice.emptyList();
        }

        public List<AccessibilityServiceInfo> getEnabledAccessibilityServiceList(int feedback, int userId) {
            return Collections.emptyList();
        }

        public List<String> getAccessibilityShortcutTargets(int type) {
            return Collections.emptyList();
        }
    }

    /** One snapshot of every window, bottom first. */
    static JSONObject snapshot(int maxNodes, int maxDepth) throws JSONException {
        Walk walk = new Walk(maxNodes, maxDepth);
        JSONArray windows = new JSONArray();
        List<View> roots = WindowManagerGlobal.getInstance().getWindowViews();
        for (int i = 0; i < roots.size(); i++) {
            View root = roots.get(i);
            JSONObject window = new JSONObject().put("index", i);
            if (root.getLayoutParams() instanceof WindowManager.LayoutParams) {
                WindowManager.LayoutParams params = (WindowManager.LayoutParams) root.getLayoutParams();
                window.put("title", cut(params.getTitle())).put("type", params.type);
            }
            int[] at = new int[2];
            root.getLocationOnScreen(at);
            window.put("bounds", bounds(at[0], at[1], at[0] + root.getWidth(), at[1] + root.getHeight()));
            if (root.hasWindowFocus()) window.put("focused", true);
            JSONObject node = walk.view(root, 0);
            if (node != null) window.put("root", node);
            windows.put(window);
        }
        return new JSONObject().put("snapshot", ++snapshots).put("nodes", walk.next - 1)
                .put("truncated", walk.truncated).put("accessibility", accessibility != null && accessibility.state != 0)
                .put("windows", windows);
    }

    private static JSONArray bounds(int left, int top, int right, int bottom) {
        return new JSONArray().put(left).put(top).put(right).put(bottom);
    }

    private static String cut(CharSequence text) {
        if (text == null || text.length() == 0) return null;
        String value = text.toString();
        return value.length() > MAX_TEXT ? value.substring(0, MAX_TEXT) : value;
    }

    /** A depth-first walk that numbers nodes and stops at the limits. */
    private static final class Walk {
        final int max;
        final int maxDepth;
        int next = 1;
        boolean truncated;

        Walk(int max, int maxDepth) {
            this.max = max;
            this.maxDepth = maxDepth;
        }

        private JSONObject open() throws JSONException {
            if (next > max) {
                truncated = true;
                return null;
            }
            return new JSONObject().put("id", next++);
        }

        JSONObject view(View view, int depth) throws JSONException {
            JSONObject json = open();
            if (json == null) return null;
            AccessibilityNodeProvider provider = view.getAccessibilityNodeProvider();
            AccessibilityNodeInfo info = null;
            try {
                info = provider != null ? provider.createAccessibilityNodeInfo(AccessibilityNodeProvider.HOST_VIEW_ID)
                        : view.createAccessibilityNodeInfo();
            } catch (RuntimeException e) {
                // A view that cannot describe itself is described from its fields.
            }
            if (info != null) {
                describe(json, info);
            } else {
                describe(json, view);
            }
            if (view.getId() != View.NO_ID && view.getResources() != null) {
                try {
                    json.put("resource_id", view.getResources().getResourceName(view.getId()));
                } catch (android.content.res.Resources.NotFoundException e) {
                    // An id made at run time has no name.
                }
            }
            JSONArray children = new JSONArray();
            if (depth + 1 >= maxDepth) {
                if (provider != null || (view instanceof ViewGroup && ((ViewGroup) view).getChildCount() > 0)) {
                    truncated = true;
                }
            } else if (provider != null && info != null) {
                virtualChildren(provider, info, depth + 1, new HashSet<>(), children);
            } else if (view instanceof ViewGroup) {
                ViewGroup group = (ViewGroup) view;
                for (int i = 0; i < group.getChildCount(); i++) {
                    JSONObject child = view(group.getChildAt(i), depth + 1);
                    if (child != null) children.put(child);
                }
            }
            if (children.length() > 0) json.put("children", children);
            return json;
        }

        private void virtualChildren(AccessibilityNodeProvider provider, AccessibilityNodeInfo parent, int depth,
                Set<Integer> seen, JSONArray into) throws JSONException {
            for (int i = 0; i < parent.getChildCount(); i++) {
                int id = AccessibilityNodeInfo.getVirtualDescendantId(parent.getChildId(i));
                // A provider's tree is the app's data: a cycle in it ends here.
                if (!seen.add(id)) continue;
                AccessibilityNodeInfo info;
                try {
                    info = provider.createAccessibilityNodeInfo(id);
                } catch (RuntimeException e) {
                    continue;
                }
                if (info == null) continue;
                JSONObject json = open();
                if (json == null) return;
                describe(json, info);
                if (info.getViewIdResourceName() != null) json.put("resource_id", cut(info.getViewIdResourceName()));
                JSONArray children = new JSONArray();
                if (depth + 1 >= maxDepth) {
                    if (info.getChildCount() > 0) truncated = true;
                } else {
                    virtualChildren(provider, info, depth + 1, seen, children);
                }
                if (children.length() > 0) json.put("children", children);
                into.put(json);
            }
        }
    }

    private static void flag(JSONObject json, String key, boolean value) throws JSONException {
        if (value) json.put(key, true);
    }

    private static void describe(JSONObject json, AccessibilityNodeInfo info) throws JSONException {
        if (info.getClassName() != null) json.put("class", info.getClassName().toString());
        json.putOpt("text", cut(info.getText()));
        json.putOpt("description", cut(info.getContentDescription()));
        json.putOpt("hint", cut(info.getHintText()));
        Rect rect = new Rect();
        info.getBoundsInScreen(rect);
        json.put("bounds", bounds(rect.left, rect.top, rect.right, rect.bottom));
        flag(json, "visible", info.isVisibleToUser());
        flag(json, "enabled", info.isEnabled());
        flag(json, "clickable", info.isClickable());
        flag(json, "long_clickable", info.isLongClickable());
        flag(json, "focusable", info.isFocusable());
        flag(json, "focused", info.isFocused());
        flag(json, "checkable", info.isCheckable());
        flag(json, "checked", info.isChecked());
        flag(json, "selected", info.isSelected());
        flag(json, "scrollable", info.isScrollable());
        flag(json, "editable", info.isEditable());
        flag(json, "password", info.isPassword());
        flag(json, "heading", info.isHeading());
    }

    /** A view's own fields, for one that cannot make an AccessibilityNodeInfo. */
    private static void describe(JSONObject json, View view) throws JSONException {
        json.put("class", view.getClass().getName());
        if (view instanceof android.widget.TextView) {
            android.widget.TextView text = (android.widget.TextView) view;
            // A password's characters never leave the app, as accessibility masks them too.
            boolean password = text.getTransformationMethod()
                    instanceof android.text.method.PasswordTransformationMethod;
            if (!password) json.putOpt("text", cut(text.getText()));
            flag(json, "password", password);
            json.putOpt("hint", cut(text.getHint()));
            flag(json, "editable", text.onCheckIsTextEditor());
        }
        json.putOpt("description", cut(view.getContentDescription()));
        int[] at = new int[2];
        view.getLocationOnScreen(at);
        json.put("bounds", bounds(at[0], at[1], at[0] + view.getWidth(), at[1] + view.getHeight()));
        flag(json, "visible", view.isShown());
        flag(json, "enabled", view.isEnabled());
        flag(json, "clickable", view.isClickable());
        flag(json, "focusable", view.isFocusable());
        flag(json, "focused", view.isFocused());
        flag(json, "selected", view.isSelected());
        flag(json, "scrollable", view.canScrollVertically(1) || view.canScrollVertically(-1)
                || view.canScrollHorizontally(1) || view.canScrollHorizontally(-1));
    }
}
