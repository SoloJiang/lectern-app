package com.example.stock;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

public class Inventory implements Iterable<Item> {
    public static final int MAX_ITEMS = 500;
    private final List<Item> items = new ArrayList<>();
    protected Map<String, Integer> counts;

    public Inventory() {
        this.counts = Map.of();
    }

    public void add(Item item) {
        add(item, 1);
    }

    public void add(Item item, int quantity) {
        for (int i = 0; i < quantity; i++) {
            items.add(item);
        }
        log("added " + item.name());
    }

    private void log(String message) {
        String line = "[inventory] " + message;
        System.out.println(line);
    }

    @Override
    public java.util.Iterator<Item> iterator() {
        return items.iterator();
    }

    public int total(Map<String, List<Item>> groups, boolean includeEmpty) {
        int sum = 0;
        for (List<Item> group : groups.values()) {
            sum += group.size();
        }
        return sum;
    }

    interface Listener {
        void changed(Inventory inventory);
    }

    enum Kind {
        TOOL,
        PART;

        boolean isTool() {
            return this == TOOL;
        }
    }

    record Item(String name, Kind kind) {
        Item {
            if (name.isEmpty()) {
                throw new IllegalArgumentException("name");
            }
        }
    }

    private static class Counter {
        private int value;

        void increment() {
            value++;
        }
    }
}
