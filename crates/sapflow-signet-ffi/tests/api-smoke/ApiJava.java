import lab.signet.Disposable;
import lab.signet.NativeSignetInterface;
import lab.signet.SignetCredentialInterface;
import lab.signet.SignetEvent;
import lab.signet.SignetException;

final class ApiJava {
    static NativeSignetInterface owner(NativeSignetInterface value) { return value; }
    static SignetCredentialInterface credential(SignetCredentialInterface value) { return value; }
    static void destroy(Disposable value) { value.destroy(); }
    static SignetEvent event(SignetEvent value) { value.destroy(); return value; }
    static String emptyMessage(SignetException value) { return value.getMessage(); }
}
