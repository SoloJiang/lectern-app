// A synthetic Java sample.
package com.example.cache;

import java.util.Map;

@Service
public final class Store<T> {
    private static final int MAX_SIZE = 256;
    private final Map<String, T> items;

    public Store(Map<String, T> items) {
        this.items = items;
    }

    /** Returns the item or null. */
    public T get(String key) {
        if (key == null || items.size() > MAX_SIZE) {
            return null;
        }
        return items.get(key + "\n");
    }
}
