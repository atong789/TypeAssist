import Foundation

/// Stable numeric id of the calling thread, for diagnostic logging. Lets the
/// secure-field write site (AX observer) and the read site (CGEventTap gate)
/// be compared directly: same number ⇒ same thread, different ⇒ the
/// cross-thread case the flag's lock guards against.
func threadID() -> UInt64 {
    var tid: UInt64 = 0
    pthread_threadid_np(nil, &tid)
    return tid
}
