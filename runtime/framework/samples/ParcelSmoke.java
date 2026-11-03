import android.content.ComponentName;
import android.content.Intent;
import android.graphics.Point;
import android.graphics.Rect;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.IInterface;
import android.os.Parcel;
import android.os.ParcelFileDescriptor;
import android.os.ResultReceiver;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Arrays;

/**
 * Passes framework Parcelables through android.os.Parcel, whose natives are uxdroid's. Prints one
 * line per check; parcel_smoke.sh compares them.
 */
public class ParcelSmoke {
    public static void main(String[] args) throws Exception {
        dev.uxdroid.system.Boot.start(args[0], args[1]);
        primitives();
        bundle();
        intent();
        fileDescriptor();
        binder();
        aidl();
        System.out.println("allocations " + (Parcel.getGlobalAllocCount() >= 0));
    }

    /** Bytes of a parcel, read back into a fresh one positioned at its start. */
    static Parcel copy(Parcel from) {
        byte[] bytes = from.marshall();
        Parcel to = Parcel.obtain();
        to.unmarshall(bytes, 0, bytes.length);
        to.setDataPosition(0);
        return to;
    }

    static void primitives() {
        Parcel p = Parcel.obtain();
        p.writeInt(-7);
        p.writeLong(1L << 40);
        p.writeFloat(1.5f);
        p.writeDouble(-2.25);
        p.writeString("h\u00e9llo \u263a \ud83d\ude00");
        p.writeString8("ascii");
        p.writeString(null);
        p.writeByteArray(new byte[] {1, 2, 3});
        p.writeBlob(new byte[] {4, 5});
        Parcel q = copy(p);
        System.out.println("primitives " + q.readInt() + " " + q.readLong() + " " + q.readFloat() + " "
                + q.readDouble() + " " + q.readString() + " " + q.readString8() + " " + q.readString()
                + " " + Arrays.toString(q.createByteArray()) + " " + Arrays.toString(q.readBlob())
                + " avail " + q.dataAvail());
        p.recycle();
        q.recycle();
    }

    static Bundle sample() {
        Bundle inner = new Bundle();
        inner.putLong("long", 42L);
        Bundle b = new Bundle();
        b.putString("string", "valor");
        b.putInt("int", 7);
        b.putIntArray("ints", new int[] {1, 2, 3});
        ArrayList<String> list = new ArrayList<>(Arrays.asList("a", "b"));
        b.putStringArrayList("list", list);
        b.putParcelable("rect", new Rect(1, 2, 3, 4));
        ArrayList<Point> points = new ArrayList<>(Arrays.asList(new Point(5, 6), new Point(7, 8)));
        b.putParcelableArrayList("points", points);
        b.putBundle("inner", inner);
        return b;
    }

    static String describe(Bundle b) {
        b.setClassLoader(ParcelSmoke.class.getClassLoader());
        return b.getString("string") + " " + b.getInt("int") + " " + Arrays.toString(b.getIntArray("ints"))
                + " " + b.getStringArrayList("list") + " " + b.getParcelable("rect", Rect.class) + " "
                + b.getParcelableArrayList("points", Point.class) + " "
                + b.getBundle("inner").getLong("long");
    }

    static void bundle() {
        Parcel p = Parcel.obtain();
        p.writeBundle(sample());
        Parcel q = copy(p);
        Bundle read = q.readBundle(ParcelSmoke.class.getClassLoader());
        // Copying a Bundle that is still parcelled copies its bytes with Parcel.appendFrom.
        Bundle copied = new Bundle(read);
        System.out.println("bundle " + describe(read));
        System.out.println("bundle copy " + describe(copied));
        p.recycle();
        q.recycle();
    }

    static void intent() {
        Intent intent = new Intent("dev.uxdroid.ACTION");
        intent.setComponent(new ComponentName("dev.uxdroid", "dev.uxdroid.Target"));
        intent.putExtra("extra", "x");
        intent.putExtra("nested", sample());
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        Parcel p = Parcel.obtain();
        p.writeParcelable(intent, 0);
        Parcel q = copy(p);
        Intent read = q.readParcelable(ParcelSmoke.class.getClassLoader(), Intent.class);
        read.setExtrasClassLoader(ParcelSmoke.class.getClassLoader());
        System.out.println("intent " + read.getAction() + " " + read.getComponent().flattenToShortString()
                + " " + Integer.toHexString(read.getFlags()) + " " + read.getStringExtra("extra") + " "
                + describe(read.getBundleExtra("nested")));
        p.recycle();
        q.recycle();
    }

    static void fileDescriptor() throws Exception {
        ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
        Parcel p = Parcel.obtain();
        p.writeParcelable(pipe[1], 0);
        System.out.println("fds " + p.hasFileDescriptors());
        try {
            p.marshall();
            System.out.println("marshalled a file descriptor");
        } catch (RuntimeException expected) {
            System.out.println("marshall refused: " + expected.getMessage());
        }
        p.setDataPosition(0);
        ParcelFileDescriptor read = p.readParcelable(null, ParcelFileDescriptor.class);
        pipe[1].close();
        p.recycle();
        try (FileOutputStream out = new ParcelFileDescriptor.AutoCloseOutputStream(read)) {
            out.write("por el pipe".getBytes("UTF-8"));
        }
        byte[] buffer = new byte[64];
        int length;
        try (FileInputStream in = new ParcelFileDescriptor.AutoCloseInputStream(pipe[0])) {
            length = in.read(buffer);
        }
        System.out.println("pipe " + new String(buffer, 0, length, "UTF-8"));
    }

    static void binder() {
        Binder token = new Binder("token");
        Bundle b = new Bundle();
        b.putBinder("token", token);
        Parcel p = Parcel.obtain();
        p.writeBundle(b);
        System.out.println("binders " + p.hasBinders());
        p.setDataPosition(0);
        Bundle read = p.readBundle();
        System.out.println("binder same " + (read.getBinder("token") == token));
        p.recycle();
    }

    /** The framework's generated AIDL proxy, transacting with its stub through parcels. */
    static void aidl() throws Exception {
        ResultReceiver receiver = new ResultReceiver(null) {
            @Override
            protected void onReceiveResult(int code, Bundle data) {
                data.setClassLoader(ParcelSmoke.class.getClassLoader());
                System.out.println("aidl result " + code + " " + describe(data));
            }
        };
        Parcel p = Parcel.obtain();
        receiver.writeToParcel(p, 0);
        p.setDataPosition(0);
        IBinder stub = p.readStrongBinder();
        p.recycle();
        Class<?> proxyClass = Class.forName("com.android.internal.os.IResultReceiver$Stub$Proxy");
        Constructor<?> constructor = proxyClass.getDeclaredConstructor(IBinder.class);
        constructor.setAccessible(true);
        IInterface proxy = (IInterface) constructor.newInstance(stub);
        Method send = proxyClass.getMethod("send", int.class, Bundle.class);
        send.invoke(proxy, 3, sample());
        // A call with the wrong interface token is refused by the stub's enforceInterface.
        Parcel data = Parcel.obtain();
        data.writeInterfaceToken("dev.uxdroid.Wrong");
        try {
            stub.transact(IBinder.FIRST_CALL_TRANSACTION, data, null, 0);
            System.out.println("aidl accepted a wrong interface");
        } catch (SecurityException expected) {
            System.out.println("aidl refused: " + expected.getMessage());
        }
        data.recycle();
    }
}
