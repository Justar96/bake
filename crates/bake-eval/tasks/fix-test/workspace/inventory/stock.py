"""Stock levels for a small warehouse."""


class InsufficientStock(Exception):
    """Raised when a request asks for more stock than there is."""


class Stock:
    def __init__(self):
        self._on_hand = {}
        self._reserved = {}

    def receive(self, sku, quantity):
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        self._on_hand[sku] = self.on_hand(sku) + quantity

    def on_hand(self, sku):
        return self._on_hand.get(sku, 0)

    def reserved(self, sku):
        return self._reserved.get(sku, 0)

    def available(self, sku):
        """Units on the shelf that no reservation holds."""
        return self.on_hand(sku) - self.reserved(sku)

    def reserve(self, sku, quantity):
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        if quantity > self.on_hand(sku):
            raise InsufficientStock(f"{sku}: wanted {quantity}, have {self.available(sku)}")
        self._reserved[sku] = self.reserved(sku) + quantity

    def release(self, sku, quantity):
        """Gives reserved units back; releasing more than is reserved is an error."""
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        self._reserved[sku] = max(0, self.reserved(sku) - quantity)

    def ship(self, sku, quantity):
        """Ships reserved units: they leave the shelf and the reservation."""
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        if quantity > self.reserved(sku):
            raise InsufficientStock(f"{sku}: only {self.reserved(sku)} reserved")
        self._on_hand[sku] = self.on_hand(sku) - quantity
        self._reserved[sku] = self.reserved(sku) - quantity
