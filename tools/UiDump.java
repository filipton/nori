package dev.nori.tools;

import android.app.UiAutomation;
import android.graphics.Rect;
import android.os.HandlerThread;
import android.util.Xml;
import android.view.accessibility.AccessibilityNodeInfo;
import java.lang.reflect.Constructor;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import org.xmlpull.v1.XmlSerializer;

/** Accessibility snapshot without uiautomator's idle wait on an updating player. */
public final class UiDump {
    private static void node(XmlSerializer xml, AccessibilityNodeInfo n, int index) throws Exception {
        xml.startTag(null, "node");
        xml.attribute(null, "index", Integer.toString(index));
        xml.attribute(null, "text", n.getText() == null ? "" : n.getText().toString());
        xml.attribute(null, "content-desc", n.getContentDescription() == null ? "" : n.getContentDescription().toString());
        xml.attribute(null, "resource-id", n.getViewIdResourceName() == null ? "" : n.getViewIdResourceName());
        xml.attribute(null, "class", n.getClassName() == null ? "" : n.getClassName().toString());
        xml.attribute(null, "package", n.getPackageName() == null ? "" : n.getPackageName().toString());
        xml.attribute(null, "clickable", Boolean.toString(n.isClickable()));
        xml.attribute(null, "enabled", Boolean.toString(n.isEnabled()));
        xml.attribute(null, "selected", Boolean.toString(n.isSelected()));
        Rect bounds = new Rect();
        n.getBoundsInScreen(bounds);
        xml.attribute(null, "bounds", bounds.toShortString());
        for (int i = 0; i < n.getChildCount(); i++) {
            AccessibilityNodeInfo child = n.getChild(i);
            if (child != null && child.isVisibleToUser()) node(xml, child, i);
        }
        xml.endTag(null, "node");
    }

    @SuppressWarnings("deprecation")
    public static void main(String[] args) throws Exception {
        android.os.Looper.prepareMainLooper();
        HandlerThread thread = new HandlerThread("nori-ui-dump");
        thread.start();
        Class<?> connectionType = Class.forName("android.app.IUiAutomationConnection");
        Object connection = Class.forName("android.app.UiAutomationConnection").getConstructor().newInstance();
        Constructor<UiAutomation> constructor = UiAutomation.class.getConstructor(android.os.Looper.class, connectionType);
        UiAutomation automation = constructor.newInstance(thread.getLooper(), connection);
        try {
            UiAutomation.class.getMethod("connect", int.class).invoke(automation, UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES);
            Semaphore changed = new Semaphore(0);
            automation.setOnAccessibilityEventListener(event -> changed.release());
            android.accessibilityservice.AccessibilityServiceInfo info = automation.getServiceInfo();
            info.flags |= android.accessibilityservice.AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
            info.flags |= android.accessibilityservice.AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS;
            automation.setServiceInfo(info);
            long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5);
            AccessibilityNodeInfo root;
            while ((root = automation.getRootInActiveWindow()) == null) {
                long left = deadline - System.nanoTime();
                if (left <= 0 || !changed.tryAcquire(left, TimeUnit.NANOSECONDS)) {
                    System.err.println("no active accessibility window");
                    System.exit(1);
                }
            }
            XmlSerializer xml = Xml.newSerializer();
            xml.setOutput(System.out, "UTF-8");
            xml.startDocument("UTF-8", true);
            xml.startTag(null, "hierarchy");
            node(xml, root, 0);
            xml.endTag(null, "hierarchy");
            xml.endDocument();
            xml.flush();
        } finally {
            UiAutomation.class.getMethod("disconnect").invoke(automation);
            thread.quitSafely();
        }
    }
}
